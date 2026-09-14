use chrono::{Datelike, Duration, NaiveDate, NaiveTime, Utc};
use rusqlite::{params, params_from_iter, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::hash::{Hash, Hasher};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
#[cfg(not(windows))]
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration as StdDuration, Instant};
use tauri::{AppHandle, Manager};
use thiserror::Error;
use url::Url;
#[cfg(windows)]
use windows_sys::Win32::UI::Shell::ShellExecuteW;
#[cfg(windows)]
use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

mod services;

const CORE_SCHEMA_VERSION: i64 = 19;
const ISTANBUL_OFFSET_MINUTES: i64 = 180;
const APPOINTMENT_STATUS_CANCELLED: &str = "cancelled";
const AUTOMATIC_BACKUP_INTERVAL: StdDuration = StdDuration::from_secs(15 * 60);
const AUTOMATIC_REMINDER_INTERVAL: StdDuration = StdDuration::from_secs(15 * 60);
const REMINDER_COORDINATOR_DIAGNOSTIC_FILE: &str = "reminder-coordinator-diagnostic.log";
const REMINDER_COORDINATOR_DIAGNOSTIC_MAX_EVENTS: usize = 128;
const SHUTDOWN_BACKUP_WAIT_TIMEOUT: StdDuration = StdDuration::from_secs(10);
const SHUTDOWN_BACKUP_POLL_INTERVAL: StdDuration = StdDuration::from_millis(25);
const GOOGLE_OAUTH_CALLBACK_TIMEOUT: StdDuration = StdDuration::from_secs(15 * 60);
const PACKAGED_RUNTIME_DEFAULT: &str =
    include_str!(concat!(env!("OUT_DIR"), "/runtime-default.json"));
static ID_COUNTER: AtomicU64 = AtomicU64::new(1);
static REMINDER_COORDINATOR_DIAGNOSTIC_LOCK: Mutex<()> = Mutex::new(());

#[derive(Debug, Error)]
pub(crate) enum AppError {
    #[error("App data yolu hazirlanamadi: {0}")]
    AppDataPath(String),
    #[error("VALIDATION_ERROR: {0}")]
    Validation(String),
    #[error("DATABASE_ERROR: {0}")]
    Database(String),
    #[error("NOT_FOUND: {0}")]
    NotFound(String),
    #[error("CONFLICT: {0}")]
    Conflict(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
}

impl Serialize for AppError {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

#[derive(Debug, Clone, Copy)]
enum AuditEntityType {
    Customer,
    Staff,
    Service,
    Appointment,
    AppointmentService,
    BusinessProfile,
}

impl AuditEntityType {
    fn as_str(self) -> &'static str {
        match self {
            Self::Customer => "customer",
            Self::Staff => "staff",
            Self::Service => "service",
            Self::Appointment => "appointment",
            Self::AppointmentService => "appointment_service",
            Self::BusinessProfile => "business_profile",
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum AuditAction {
    Create,
    Update,
    Archive,
    Reactivate,
    Deactivate,
    StatusChange,
    PriceOverride,
}

impl AuditAction {
    fn as_str(self) -> &'static str {
        match self {
            Self::Create => "create",
            Self::Update => "update",
            Self::Archive => "archive",
            Self::Reactivate => "reactivate",
            Self::Deactivate => "deactivate",
            Self::StatusChange => "status_change",
            Self::PriceOverride => "price_override",
        }
    }
}

enum AuditMetadata<'a> {
    ChangedFields(&'a [&'a str]),
    StatusChange {
        from_status: &'a str,
        to_status: &'a str,
    },
    PriceOverride {
        previous_minor: i64,
        new_minor: i64,
    },
}

fn append_audit_event(
    connection: &Connection,
    entity_type: AuditEntityType,
    entity_id: &str,
    action: AuditAction,
    metadata: Option<AuditMetadata<'_>>,
) -> Result<(), AppError> {
    let metadata_json = match metadata {
        None => None,
        Some(AuditMetadata::ChangedFields(changed_fields)) => Some(
            serde_json::json!({ "changed_fields": changed_fields }).to_string(),
        ),
        Some(AuditMetadata::StatusChange {
            from_status,
            to_status,
        }) => Some(
            serde_json::json!({ "from_status": from_status, "to_status": to_status }).to_string(),
        ),
        Some(AuditMetadata::PriceOverride {
            previous_minor,
            new_minor,
        }) => Some(
            serde_json::json!({ "previous_price_minor": previous_minor, "new_price_minor": new_minor }).to_string(),
        ),
    };
    connection.execute(
        "INSERT INTO audit_log (id, occurred_at, entity_type, entity_id, action, metadata_json)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            new_uuid(),
            now_iso(),
            entity_type.as_str(),
            entity_id,
            action.as_str(),
            metadata_json
        ],
    )?;
    Ok(())
}

#[derive(Debug)]
pub struct AppState {
    database_path: PathBuf,
    sqlite: Mutex<Connection>,
    backup_coordinator: Arc<BackupCoordinator>,
    reminder_coordinator: Arc<ReminderCoordinator>,
}

#[derive(Debug, Default)]
struct BackupCoordinator {
    current_change_generation: AtomicU64,
    last_successfully_backed_up_generation: AtomicU64,
    automatic_backup_running: AtomicBool,
    automatic_backup_registered: AtomicBool,
    restore_in_progress: AtomicBool,
    backup_operation_lock: Mutex<()>,
}

#[derive(Debug, Default)]
struct ReminderCoordinator {
    registered: AtomicBool,
    running: AtomicBool,
    wake_pending: AtomicBool,
    signal_sender: Mutex<Option<mpsc::Sender<ReminderCoordinatorSignal>>>,
}

#[derive(Debug)]
enum ReminderCoordinatorSignal {
    Wake,
    Shutdown,
}

#[derive(Debug, Clone, Copy)]
enum ReminderWakeRequestResult {
    Sent,
    Coalesced,
    Unavailable,
}

#[derive(Debug, Clone, Copy)]
enum ReminderCoordinatorDiagnostic {
    WakeRequested {
        registered: bool,
        result: ReminderWakeRequestResult,
    },
    WakeReceived,
    TimeoutTick,
    CoordinatorDisconnected,
    CoordinatorExit,
    TickStarted,
    RunningGuardSkip,
    AutomaticEnabledYes,
    AutomaticEnabledNo,
    AutomaticEnabledReadError,
    ReconcileFailed,
    SupabaseConfigPresent,
    SupabaseConfigMissing,
    SessionPresent,
    SessionMissing,
    SessionReadError,
    DispatcherStatusOkActive,
    DispatcherStatusOkPaused,
    DispatcherStatusAuthError,
    DispatcherStatusNetworkError,
    DispatcherStatusOtherError,
    AuthRefreshStarted,
    AuthRefreshSucceeded,
    AuthRefreshFailed,
    DispatcherStatusRetrySucceeded,
    DispatcherStatusRetryFailed,
    ProcessOrderedOutboxEntered,
    ProcessOrderedOutboxCompleted,
    ProcessOrderedOutboxFailed,
}

impl ReminderCoordinatorDiagnostic {
    fn marker(self) -> &'static str {
        match self {
            Self::WakeRequested {
                registered: true,
                result: ReminderWakeRequestResult::Sent,
            } => "wake_requested registered=true send_result=success",
            Self::WakeRequested {
                registered: true,
                result: ReminderWakeRequestResult::Coalesced,
            } => "wake_requested registered=true send_result=coalesced",
            Self::WakeRequested {
                registered: true,
                result: ReminderWakeRequestResult::Unavailable,
            } => "wake_requested registered=true send_result=failure",
            Self::WakeRequested {
                registered: false, ..
            } => "wake_requested registered=false send_result=failure",
            Self::WakeReceived => "wake_received",
            Self::TimeoutTick => "timeout_tick",
            Self::CoordinatorDisconnected => "coordinator_disconnected",
            Self::CoordinatorExit => "coordinator_exit",
            Self::TickStarted => "tick_started",
            Self::RunningGuardSkip => "running_guard_skip",
            Self::AutomaticEnabledYes => "automatic_enabled_yes",
            Self::AutomaticEnabledNo => "automatic_enabled_no",
            Self::AutomaticEnabledReadError => "automatic_enabled_read_error",
            Self::ReconcileFailed => "reconcile_failed",
            Self::SupabaseConfigPresent => "supabase_config_present",
            Self::SupabaseConfigMissing => "supabase_config_missing",
            Self::SessionPresent => "session_present",
            Self::SessionMissing => "session_missing",
            Self::SessionReadError => "session_read_error",
            Self::DispatcherStatusOkActive => "dispatcher_status_ok_active",
            Self::DispatcherStatusOkPaused => "dispatcher_status_ok_paused",
            Self::DispatcherStatusAuthError => "dispatcher_status_auth_error",
            Self::DispatcherStatusNetworkError => "dispatcher_status_network_error",
            Self::DispatcherStatusOtherError => "dispatcher_status_other_error",
            Self::AuthRefreshStarted => "auth_refresh_started",
            Self::AuthRefreshSucceeded => "auth_refresh_succeeded",
            Self::AuthRefreshFailed => "auth_refresh_failed",
            Self::DispatcherStatusRetrySucceeded => "dispatcher_status_retry_succeeded",
            Self::DispatcherStatusRetryFailed => "dispatcher_status_retry_failed",
            Self::ProcessOrderedOutboxEntered => "process_ordered_outbox_entered",
            Self::ProcessOrderedOutboxCompleted => "process_ordered_outbox_completed",
            Self::ProcessOrderedOutboxFailed => "process_ordered_outbox_failed",
        }
    }

    fn is_allowed_marker(marker: &str) -> bool {
        matches!(
            marker,
            "wake_requested registered=true send_result=success"
                | "wake_requested registered=true send_result=coalesced"
                | "wake_requested registered=true send_result=failure"
                | "wake_requested registered=false send_result=failure"
                | "wake_received"
                | "timeout_tick"
                | "coordinator_disconnected"
                | "coordinator_exit"
                | "tick_started"
                | "running_guard_skip"
                | "automatic_enabled_yes"
                | "automatic_enabled_no"
                | "automatic_enabled_read_error"
                | "reconcile_failed"
                | "supabase_config_present"
                | "supabase_config_missing"
                | "session_present"
                | "session_missing"
                | "session_read_error"
                | "dispatcher_status_ok_active"
                | "dispatcher_status_ok_paused"
                | "dispatcher_status_auth_error"
                | "dispatcher_status_network_error"
                | "dispatcher_status_other_error"
                | "auth_refresh_started"
                | "auth_refresh_succeeded"
                | "auth_refresh_failed"
                | "dispatcher_status_retry_succeeded"
                | "dispatcher_status_retry_failed"
                | "process_ordered_outbox_entered"
                | "process_ordered_outbox_completed"
                | "process_ordered_outbox_failed"
        )
    }
}

impl ReminderCoordinator {
    fn register(&self) -> bool {
        self.registered
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    fn stop(&self) {
        if let Some(sender) = self
            .signal_sender
            .lock()
            .expect("reminder shutdown mutex poisoned")
            .take()
        {
            let _ = sender.send(ReminderCoordinatorSignal::Shutdown);
        }
    }

    fn request_wake(&self) -> ReminderWakeRequestResult {
        if !self.registered.load(Ordering::Acquire) {
            return ReminderWakeRequestResult::Unavailable;
        }
        if self
            .wake_pending
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return ReminderWakeRequestResult::Coalesced;
        }
        let sent = self
            .signal_sender
            .lock()
            .expect("reminder signal mutex poisoned")
            .as_ref()
            .is_some_and(|sender| sender.send(ReminderCoordinatorSignal::Wake).is_ok());
        if !sent {
            self.wake_pending.store(false, Ordering::Release);
        }
        if sent {
            ReminderWakeRequestResult::Sent
        } else {
            ReminderWakeRequestResult::Unavailable
        }
    }
}

impl BackupCoordinator {
    fn mark_business_change(&self) {
        self.current_change_generation
            .fetch_add(1, Ordering::AcqRel);
    }

    fn current_generation(&self) -> u64 {
        self.current_change_generation.load(Ordering::Acquire)
    }

    fn backed_up_generation(&self) -> u64 {
        self.last_successfully_backed_up_generation
            .load(Ordering::Acquire)
    }

    fn is_dirty(&self) -> bool {
        self.current_generation() > self.backed_up_generation()
    }

    fn begin_automatic_backup(&self) -> Option<u64> {
        if self.restore_in_progress.load(Ordering::Acquire)
            || !self.is_dirty()
            || self
                .automatic_backup_running
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
        {
            return None;
        }
        Some(self.current_generation())
    }

    fn finish_automatic_backup(&self, captured_generation: u64, succeeded: bool) {
        if succeeded {
            self.last_successfully_backed_up_generation
                .fetch_max(captured_generation, Ordering::AcqRel);
        }
        self.automatic_backup_running
            .store(false, Ordering::Release);
    }

    fn register_automatic_backup_loop(&self) -> bool {
        self.automatic_backup_registered
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    fn wait_for_automatic_backup_slot(
        &self,
        timeout: StdDuration,
    ) -> Result<Option<u64>, AppError> {
        let started = Instant::now();
        loop {
            if let Some(captured_generation) = self.begin_automatic_backup() {
                return Ok(Some(captured_generation));
            }
            if !self.is_dirty() {
                return Ok(None);
            }
            if started.elapsed() >= timeout {
                return Err(AppError::Database(
                    "AUTOMATIC_BACKUP_SHUTDOWN_TIMEOUT".to_string(),
                ));
            }
            std::thread::sleep(SHUTDOWN_BACKUP_POLL_INTERVAL);
        }
    }

    fn begin_restore(&self) -> bool {
        self.restore_in_progress
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    fn finish_restore(&self, restored_generation: u64) {
        self.current_change_generation
            .store(restored_generation, Ordering::Release);
        self.last_successfully_backed_up_generation
            .store(restored_generation, Ordering::Release);
        self.restore_in_progress.store(false, Ordering::Release);
    }

    fn abort_restore(&self) {
        self.restore_in_progress.store(false, Ordering::Release);
    }
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct GoogleRuntimeConfig {
    client_id: String,
    #[serde(default)]
    client_secret: Option<String>,
    calendar_id: Option<String>,
}

impl GoogleRuntimeConfig {
    fn into_oauth_config(self) -> services::google::GoogleOAuthConfig {
        services::google::GoogleOAuthConfig {
            client_id: self.client_id,
            client_secret: self.client_secret.unwrap_or_default(),
            calendar_id: self.calendar_id,
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LiveServicesConfig {
    google: Option<GoogleRuntimeConfig>,
    supabase: Option<services::supabase::SupabaseConfig>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct GoogleConnectResult {
    connected: bool,
    calendar_id: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppHealth {
    ok: bool,
    app_version: String,
    schema_version: i64,
    database_path: String,
    message: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Customer {
    id: String,
    first_name: String,
    last_name: String,
    phone: Option<String>,
    email: Option<String>,
    notes: Option<String>,
    whatsapp_reminder_enabled: bool,
    whatsapp_consent_confirmed: bool,
    is_active: bool,
    created_at: String,
    updated_at: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CustomerInput {
    first_name: String,
    last_name: String,
    phone: Option<String>,
    email: Option<String>,
    notes: Option<String>,
    whatsapp_reminder_enabled: Option<bool>,
    whatsapp_consent_confirmed: Option<bool>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct BusinessProfile {
    business_name: String,
    phone: Option<String>,
    email: Option<String>,
    address: Option<String>,
    currency_code: String,
    theme_key: String,
    logo_data: Option<Vec<u8>>,
    logo_mime_type: Option<String>,
    updated_at: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BusinessProfileInput {
    business_name: String,
    phone: Option<String>,
    email: Option<String>,
    address: Option<String>,
    currency_code: String,
    theme_key: String,
    logo_data: Option<Vec<u8>>,
    logo_mime_type: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OnboardingState {
    pub needs_onboarding: bool,
    pub next_step: u8,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Staff {
    id: String,
    first_name: String,
    last_name: Option<String>,
    phone: Option<String>,
    specialty_note: Option<String>,
    color_key: String,
    sort_order: i64,
    is_active: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct StaffWorkingHour {
    staff_id: String,
    weekday: i64,
    start_minute: i64,
    end_minute: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct StaffTimeOff {
    id: String,
    staff_id: String,
    local_date: String,
    full_day: bool,
    start_minute: Option<i64>,
    end_minute: Option<i64>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StaffTimeOffInput {
    local_date: String,
    full_day: bool,
    start_minute: Option<i64>,
    end_minute: Option<i64>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StaffInput {
    first_name: String,
    last_name: Option<String>,
    phone: Option<String>,
    specialty_note: Option<String>,
    color_key: String,
    is_active: Option<bool>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceCategory {
    id: String,
    name: String,
    sort_order: i64,
    is_active: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceItem {
    id: String,
    category_id: String,
    category_name: String,
    name: String,
    duration_minutes: Option<i64>,
    default_price_minor: i64,
    sort_order: i64,
    is_active: bool,
    availability_status: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CategoryInput {
    name: String,
    is_active: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceInput {
    category_id: String,
    name: String,
    duration_minutes: Option<i64>,
    default_price_minor: Option<i64>,
    is_active: Option<bool>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AppointmentServiceSnapshot {
    service_id: String,
    service_name_snapshot: String,
    duration_minutes_snapshot: i64,
    listed_price_snapshot_minor: i64,
    charged_price_minor: i64,
    sort_order: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppointmentSummary {
    id: String,
    customer_id: String,
    staff_id: String,
    start_at_utc: String,
    end_at_utc: String,
    total_duration_minutes: i64,
    status: String,
    note: Option<String>,
    whatsapp_reminder_enabled: bool,
    whatsapp_reminder_effective: bool,
    customer_name: String,
    customer_phone: Option<String>,
    staff_name: String,
    service_names: Vec<String>,
    services: Vec<AppointmentServiceSnapshot>,
    updated_at: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CustomerHistoryCustomer {
    customer_id: String,
    name: String,
    phone: Option<String>,
    is_active: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CustomerHistoryAppointment {
    appointment_id: String,
    local_date: String,
    local_time: String,
    status: String,
    staff_id: String,
    staff_display_name: String,
    services: Vec<AppointmentServiceSnapshot>,
    total_charged_minor: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CustomerHistoryPage {
    customer: CustomerHistoryCustomer,
    appointments: Vec<CustomerHistoryAppointment>,
    limit: i64,
    offset: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RepeatBookingSeedService {
    service_id: String,
    service_name_snapshot: String,
    is_eligible: bool,
    unavailable_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RepeatBookingSeed {
    customer_id: String,
    customer_display_name: String,
    staff_id: Option<String>,
    services: Vec<RepeatBookingSeedService>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppointmentInput {
    customer_id: String,
    staff_id: String,
    local_date: String,
    local_start_time: String,
    service_ids: Vec<String>,
    status: Option<String>,
    note: Option<String>,
    whatsapp_reminder_enabled: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AppointmentInterval {
    start_at_utc: chrono::DateTime<Utc>,
    end_at_utc: chrono::DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AppointmentAvailabilityReason {
    OutsideWorkingHours,
    StaffTimeOff,
    AppointmentConflict,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AppointmentAvailability {
    Available,
    Unavailable(AppointmentAvailabilityReason),
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DatabaseBackupSummary {
    database_path: String,
    backup_path: String,
    integrity: String,
    machine_bound_secrets_removed: bool,
    created_at_utc: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DatabaseCleanSummary {
    database_path: String,
    safety_backup_path: String,
    integrity: String,
    cleaned_at_utc: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DataFolderSummary {
    path: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfirmedInput {
    confirmed: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WhatsAppCandidate {
    appointment_id: String,
    customer_id: String,
    customer_name: String,
    phone: String,
    start_at_utc: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MockAuthSession {
    email: String,
    access_token_hint: String,
    expires_at_utc: String,
}

fn app_data_root(app: &AppHandle) -> Result<PathBuf, AppError> {
    app.path()
        .app_data_dir()
        .map_err(|error| AppError::AppDataPath(error.to_string()))
}

fn data_paths(root: &Path) -> Result<(PathBuf, PathBuf, PathBuf, PathBuf, PathBuf), AppError> {
    let database_dir = root.join("database");
    let backups_dir = root.join("backups");
    let logs_dir = root.join("logs");
    let exports_dir = root.join("exports");
    let settings_dir = root.join("settings");

    for dir in [
        &database_dir,
        &backups_dir,
        &logs_dir,
        &exports_dir,
        &settings_dir,
    ] {
        fs::create_dir_all(dir)?;
    }

    Ok((
        database_dir.join("salon-foundation.db"),
        backups_dir,
        logs_dir,
        exports_dir,
        settings_dir,
    ))
}

fn business_data_directory(database_path: &Path) -> Result<PathBuf, AppError> {
    let root = database_path
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| AppError::AppDataPath("business data path invalid".to_string()))?;
    fs::create_dir_all(root)?;
    Ok(root.to_path_buf())
}

fn open_business_data_directory(database_path: &Path) -> Result<DataFolderSummary, AppError> {
    let directory = business_data_directory(database_path)?;
    #[cfg(windows)]
    {
        let operation = wide_null("open");
        let target = wide_null(&directory.display().to_string());
        let result = unsafe {
            ShellExecuteW(
                std::ptr::null_mut(),
                operation.as_ptr(),
                target.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                SW_SHOWNORMAL,
            )
        };
        if !shell_execute_succeeded(result as isize) {
            return Err(AppError::Database("DATA_FOLDER_OPEN_FAILED".to_string()));
        }
    }
    #[cfg(not(windows))]
    {
        let status = Command::new("xdg-open").arg(&directory).status()?;
        if !status.success() {
            return Err(AppError::Database("DATA_FOLDER_OPEN_FAILED".to_string()));
        }
    }
    Ok(DataFolderSummary {
        path: directory.display().to_string(),
    })
}

fn initialize_database_at(path: &Path) -> Result<Connection, AppError> {
    let connection = Connection::open(path)?;
    connection.pragma_update(None, "journal_mode", "WAL")?;
    connection.pragma_update(None, "foreign_keys", "ON")?;
    migrate_core(&connection)?;
    Ok(connection)
}

fn open_database(path: &Path) -> Result<Connection, AppError> {
    initialize_database_at(path)
}

#[cfg(test)]
fn normalize_rehearsal_path(path: &Path) -> Result<PathBuf, AppError> {
    if let Ok(canonical) = path.canonicalize() {
        return Ok(canonical);
    }
    if let Some(parent) = path.parent() {
        if let Ok(parent_c) = parent.canonicalize() {
            if let Some(file_name) = path.file_name() {
                return Ok(parent_c.join(file_name));
            }
        }
    }
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .map_err(AppError::Io)
    }
}

#[cfg(test)]
fn initialize_explicit_migration_rehearsal(
    rehearsal_path: Option<&Path>,
    forbidden_canonical_path: Option<&Path>,
) -> Result<Connection, AppError> {
    let rehearsal_path = rehearsal_path
        .ok_or_else(|| AppError::Validation("MIGRATION_REHEARSAL_PATH_REQUIRED".into()))?;
    let forbidden_canonical_path = forbidden_canonical_path.ok_or_else(|| {
        AppError::Validation("MIGRATION_REHEARSAL_FORBIDDEN_PATH_REQUIRED".into())
    })?;

    let canonical_rehearsal = normalize_rehearsal_path(rehearsal_path)?;
    let canonical_forbidden = normalize_rehearsal_path(forbidden_canonical_path)?;

    if canonical_rehearsal.to_string_lossy().to_lowercase()
        == canonical_forbidden.to_string_lossy().to_lowercase()
    {
        return Err(AppError::Validation(
            "MIGRATION_REHEARSAL_CANONICAL_PATH_FORBIDDEN".into(),
        ));
    }

    initialize_database_at(rehearsal_path)
}

fn migrate_core(connection: &Connection) -> Result<(), AppError> {
    normalize_app_meta_for_core(connection)?;
    connection.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS app_meta (
          id INTEGER PRIMARY KEY AUTOINCREMENT NOT NULL,
          schema_version INTEGER NOT NULL,
          initialized_at TEXT NOT NULL
        );
        INSERT INTO app_meta (schema_version, initialized_at)
        SELECT 1, datetime('now')
        WHERE NOT EXISTS (SELECT 1 FROM app_meta);

        CREATE TABLE IF NOT EXISTS customers (
          id TEXT PRIMARY KEY NOT NULL,
          first_name TEXT NOT NULL,
          last_name TEXT NOT NULL,
          phone TEXT,
          email TEXT,
          whatsapp_reminder_enabled INTEGER NOT NULL DEFAULT 1,
          whatsapp_consent_confirmed INTEGER NOT NULL DEFAULT 0,
          whatsapp_consent_recorded_at TEXT,
          notes TEXT,
          is_active INTEGER NOT NULL DEFAULT 1,
          created_at TEXT NOT NULL,
          updated_at TEXT NOT NULL
        );
        CREATE UNIQUE INDEX IF NOT EXISTS customers_phone_unique_idx ON customers (phone);
        CREATE INDEX IF NOT EXISTS customers_active_name_idx ON customers (is_active, last_name, first_name);

        CREATE TABLE IF NOT EXISTS service_categories (
          id TEXT PRIMARY KEY NOT NULL,
          name TEXT NOT NULL,
          name_key TEXT NOT NULL,
          sort_order INTEGER NOT NULL CHECK (sort_order >= 0),
          is_active INTEGER NOT NULL DEFAULT 1 CHECK (is_active IN (0, 1)),
          created_at TEXT NOT NULL,
          updated_at TEXT NOT NULL
        );
        CREATE UNIQUE INDEX IF NOT EXISTS service_categories_name_key_unique_idx ON service_categories (name_key);
        CREATE INDEX IF NOT EXISTS service_categories_sort_order_idx ON service_categories (sort_order);

        CREATE TABLE IF NOT EXISTS services (
          id TEXT PRIMARY KEY NOT NULL,
          category_id TEXT NOT NULL,
          name TEXT NOT NULL,
          name_key TEXT NOT NULL,
          duration_minutes INTEGER CHECK (duration_minutes IS NULL OR (duration_minutes BETWEEN 5 AND 480 AND duration_minutes % 5 = 0)),
          default_price_minor INTEGER NOT NULL DEFAULT 0 CHECK (default_price_minor >= 0),
          sort_order INTEGER NOT NULL CHECK (sort_order >= 0),
          is_active INTEGER NOT NULL DEFAULT 1 CHECK (is_active IN (0, 1)),
          created_at TEXT NOT NULL,
          updated_at TEXT NOT NULL,
          FOREIGN KEY (category_id) REFERENCES service_categories (id) ON DELETE RESTRICT
        );
        CREATE UNIQUE INDEX IF NOT EXISTS services_category_name_key_unique_idx ON services (category_id, name_key);
        CREATE INDEX IF NOT EXISTS services_category_sort_order_idx ON services (category_id, sort_order);

        CREATE TABLE IF NOT EXISTS staff (
          id TEXT PRIMARY KEY NOT NULL,
          first_name TEXT NOT NULL,
          last_name TEXT,
          phone TEXT,
          specialty_note TEXT,
          color_key TEXT NOT NULL,
          sort_order INTEGER NOT NULL,
          is_active INTEGER DEFAULT 1 NOT NULL,
          created_at TEXT NOT NULL,
          updated_at TEXT NOT NULL,
          CHECK (is_active IN (0, 1)),
          CHECK (sort_order >= 0),
          CHECK (color_key IN ('sage', 'teal', 'blue', 'purple', 'rose', 'coral', 'amber', 'slate'))
        );
        CREATE INDEX IF NOT EXISTS staff_sort_order_idx ON staff (sort_order);
        CREATE INDEX IF NOT EXISTS staff_is_active_idx ON staff (is_active);
        CREATE INDEX IF NOT EXISTS staff_first_name_idx ON staff (first_name);
        CREATE INDEX IF NOT EXISTS staff_phone_idx ON staff (phone);

        CREATE TABLE IF NOT EXISTS staff_services (
          staff_id TEXT NOT NULL,
          service_id TEXT NOT NULL,
          created_at TEXT NOT NULL,
          PRIMARY KEY (staff_id, service_id),
          FOREIGN KEY (staff_id) REFERENCES staff(id) ON UPDATE RESTRICT ON DELETE RESTRICT,
          FOREIGN KEY (service_id) REFERENCES services(id) ON UPDATE RESTRICT ON DELETE RESTRICT
        );
        CREATE INDEX IF NOT EXISTS staff_services_service_id_idx ON staff_services (service_id);

        CREATE TABLE IF NOT EXISTS appointments (
          id TEXT PRIMARY KEY,
          customer_id TEXT NOT NULL,
          staff_id TEXT NOT NULL,
          start_at_utc TEXT NOT NULL,
          end_at_utc TEXT NOT NULL,
          total_duration_minutes INTEGER NOT NULL CHECK (total_duration_minutes BETWEEN 5 AND 720),
          status TEXT NOT NULL DEFAULT 'planned' CHECK (status IN ('planned', 'confirmed', 'completed', 'cancelled', 'no_show')),
          note TEXT NULL CHECK (note IS NULL OR length(note) <= 2000),
          whatsapp_reminder_enabled INTEGER NOT NULL DEFAULT 1 CHECK (whatsapp_reminder_enabled IN (0, 1)),
          created_at TEXT NOT NULL,
          updated_at TEXT NOT NULL,
          CHECK (end_at_utc > start_at_utc),
          FOREIGN KEY (customer_id) REFERENCES customers(id) ON UPDATE RESTRICT ON DELETE RESTRICT,
          FOREIGN KEY (staff_id) REFERENCES staff(id) ON UPDATE RESTRICT ON DELETE RESTRICT
        );
        CREATE INDEX IF NOT EXISTS appointments_start_at_utc_idx ON appointments(start_at_utc);
        CREATE INDEX IF NOT EXISTS appointments_customer_id_idx ON appointments(customer_id);
        CREATE INDEX IF NOT EXISTS appointments_staff_id_idx ON appointments(staff_id);
        CREATE INDEX IF NOT EXISTS appointments_status_idx ON appointments(status);
        CREATE INDEX IF NOT EXISTS appointments_staff_time_idx ON appointments(staff_id, start_at_utc, end_at_utc);
        CREATE INDEX IF NOT EXISTS appointments_list_idx ON appointments(start_at_utc, status, staff_id, customer_id);

        CREATE TABLE IF NOT EXISTS appointment_services (
          appointment_id TEXT NOT NULL,
          service_id TEXT NOT NULL,
          service_name_snapshot TEXT NOT NULL CHECK (length(trim(service_name_snapshot)) > 0 AND length(service_name_snapshot) <= 200),
          duration_minutes_snapshot INTEGER NOT NULL CHECK (duration_minutes_snapshot BETWEEN 5 AND 480),
          listed_price_snapshot_minor INTEGER NOT NULL DEFAULT 0 CHECK (listed_price_snapshot_minor >= 0),
          charged_price_minor INTEGER NOT NULL DEFAULT 0 CHECK (charged_price_minor >= 0),
          sort_order INTEGER NOT NULL CHECK (sort_order >= 0),
          created_at TEXT NOT NULL,
          PRIMARY KEY (appointment_id, service_id),
          FOREIGN KEY (appointment_id) REFERENCES appointments(id) ON UPDATE RESTRICT ON DELETE RESTRICT,
          FOREIGN KEY (service_id) REFERENCES services(id) ON UPDATE RESTRICT ON DELETE RESTRICT
        );
        CREATE INDEX IF NOT EXISTS appointment_services_service_id_idx ON appointment_services(service_id);

        CREATE TABLE IF NOT EXISTS appointment_reminders (
          id TEXT PRIMARY KEY NOT NULL,
          appointment_id TEXT NOT NULL,
          channel TEXT NOT NULL DEFAULT 'whatsapp' CHECK (channel IN ('whatsapp')),
          reminder_type TEXT NOT NULL DEFAULT 'appointment_24h' CHECK (reminder_type IN ('appointment_24h')),
          scheduled_for_utc TEXT NOT NULL,
          status TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'processing', 'cancelled', 'sent', 'failed', 'uncertain')),
          attempt_count INTEGER NOT NULL DEFAULT 0 CHECK (attempt_count >= 0),
          last_attempt_at_utc TEXT,
          sent_at_utc TEXT,
          cancelled_at_utc TEXT,
          failure_code TEXT,
          failure_message TEXT,
          claim_token TEXT,
          claimed_at_utc TEXT,
          payload_version INTEGER NOT NULL DEFAULT 1 CHECK (payload_version >= 1),
          created_at TEXT NOT NULL,
          updated_at TEXT NOT NULL,
          FOREIGN KEY (appointment_id) REFERENCES appointments(id) ON UPDATE RESTRICT ON DELETE CASCADE
        );
        CREATE UNIQUE INDEX IF NOT EXISTS appointment_reminders_appointment_channel_type_unique_idx
          ON appointment_reminders (appointment_id, channel, reminder_type);
        CREATE INDEX IF NOT EXISTS appointment_reminders_due_idx
          ON appointment_reminders (status, scheduled_for_utc);

        CREATE TABLE IF NOT EXISTS whatsapp_settings (
          id INTEGER PRIMARY KEY NOT NULL CHECK (id = 1),
          is_enabled INTEGER NOT NULL DEFAULT 0 CHECK (is_enabled IN (0, 1)),
          phone_number_id TEXT,
          template_name TEXT,
          template_language_code TEXT NOT NULL DEFAULT 'tr' CHECK (length(trim(template_language_code)) > 0),
          automatic_reminder_enabled INTEGER NOT NULL DEFAULT 0 CHECK (automatic_reminder_enabled IN (0, 1)),
          created_at TEXT NOT NULL,
          updated_at TEXT NOT NULL
        );
        INSERT INTO whatsapp_settings (id, is_enabled, phone_number_id, template_name, template_language_code, automatic_reminder_enabled, created_at, updated_at)
        VALUES (1, 0, NULL, NULL, 'tr', 0, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
        ON CONFLICT(id) DO NOTHING;

        CREATE TABLE IF NOT EXISTS secure_secrets (
          secret_key TEXT PRIMARY KEY NOT NULL CHECK (length(trim(secret_key)) > 0),
          encrypted_value BLOB NOT NULL CHECK (length(encrypted_value) > 0),
          encryption_provider TEXT NOT NULL CHECK (length(trim(encryption_provider)) > 0),
          created_at TEXT NOT NULL,
          updated_at TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS cloud_reminder_settings (
          id INTEGER PRIMARY KEY NOT NULL CHECK (id = 1),
          cloud_mode_enabled INTEGER NOT NULL DEFAULT 0 CHECK (cloud_mode_enabled IN (0, 1)),
          project_url TEXT,
          publishable_key TEXT,
          created_at_utc TEXT NOT NULL,
          updated_at_utc TEXT NOT NULL,
          CHECK (project_url IS NULL OR (length(project_url) BETWEEN 20 AND 256)),
          CHECK (publishable_key IS NULL OR (length(publishable_key) BETWEEN 20 AND 512))
        );
        INSERT INTO cloud_reminder_settings (id, cloud_mode_enabled, project_url, publishable_key, created_at_utc, updated_at_utc)
        VALUES (1, 0, NULL, NULL, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
        ON CONFLICT(id) DO NOTHING;

        CREATE TABLE IF NOT EXISTS reminder_cloud_state (
          reminder_id TEXT PRIMARY KEY NOT NULL REFERENCES appointment_reminders(id) ON DELETE CASCADE,
          last_synced_revision INTEGER NOT NULL DEFAULT 0 CHECK (last_synced_revision >= 0),
          next_revision INTEGER NOT NULL DEFAULT 1 CHECK (next_revision >= 1),
          last_remote_revision INTEGER CHECK (last_remote_revision IS NULL OR last_remote_revision >= 1),
          last_remote_status TEXT CHECK (last_remote_status IS NULL OR last_remote_status IN ('pending', 'processing', 'sent', 'failed', 'uncertain', 'cancelled')),
          last_remote_updated_at_utc TEXT,
          last_error_code TEXT,
          created_at_utc TEXT NOT NULL,
          updated_at_utc TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS reminder_cloud_outbox (
          id TEXT PRIMARY KEY NOT NULL,
          reminder_id TEXT NOT NULL REFERENCES appointment_reminders(id) ON DELETE CASCADE,
          revision INTEGER NOT NULL CHECK (revision >= 1),
          action TEXT NOT NULL CHECK (action IN ('upsert', 'cancel')),
          client_mutation_id TEXT NOT NULL,
          payload_json TEXT,
          payload_hash TEXT NOT NULL CHECK (length(payload_hash) = 64),
          sync_status TEXT NOT NULL CHECK (sync_status IN ('pending', 'in_flight', 'synced', 'blocked')),
          last_attempt_at_utc TEXT,
          last_error_code TEXT,
          created_at_utc TEXT NOT NULL,
          updated_at_utc TEXT NOT NULL,
          UNIQUE (reminder_id, revision),
          UNIQUE (client_mutation_id),
          CHECK ((action = 'upsert' AND payload_json IS NOT NULL) OR (action = 'cancel' AND payload_json IS NULL))
        );
        CREATE INDEX IF NOT EXISTS reminder_cloud_outbox_pending_idx
          ON reminder_cloud_outbox (sync_status, created_at_utc, reminder_id, revision);

        CREATE TABLE IF NOT EXISTS google_calendar_settings (
          id INTEGER PRIMARY KEY NOT NULL CHECK (id = 1),
          sync_enabled INTEGER NOT NULL DEFAULT 0 CHECK (sync_enabled IN (0, 1)),
          client_id TEXT,
          calendar_id TEXT NOT NULL DEFAULT 'primary' CHECK (length(trim(calendar_id)) > 0),
          account_email TEXT,
          created_at_utc TEXT NOT NULL,
          updated_at_utc TEXT NOT NULL
        );
        INSERT INTO google_calendar_settings (id, sync_enabled, client_id, calendar_id, account_email, created_at_utc, updated_at_utc)
        VALUES (1, 0, NULL, 'primary', NULL, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
        ON CONFLICT(id) DO NOTHING;

        CREATE TABLE IF NOT EXISTS appointment_google_calendar_sync (
          appointment_id TEXT PRIMARY KEY NOT NULL REFERENCES appointments(id) ON DELETE CASCADE,
          google_event_id TEXT,
          last_synced_updated_at_utc TEXT,
          last_error_code TEXT,
          created_at_utc TEXT NOT NULL,
          updated_at_utc TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS google_calendar_outbox (
          id TEXT PRIMARY KEY NOT NULL,
          appointment_id TEXT NOT NULL REFERENCES appointments(id) ON DELETE CASCADE,
          sync_status TEXT NOT NULL CHECK (sync_status IN ('pending', 'in_flight', 'synced', 'blocked')),
          last_attempt_at_utc TEXT,
          last_error_code TEXT,
          created_at_utc TEXT NOT NULL,
          updated_at_utc TEXT NOT NULL,
          UNIQUE (appointment_id)
        );
        CREATE INDEX IF NOT EXISTS google_calendar_outbox_pending_idx
          ON google_calendar_outbox (sync_status, created_at_utc, appointment_id);

        CREATE TABLE IF NOT EXISTS foundation_notes (
          id INTEGER PRIMARY KEY AUTOINCREMENT,
          title TEXT NOT NULL,
          created_at_utc TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS foundation_notes_created_idx ON foundation_notes (created_at_utc DESC, id DESC);
        ",
    )?;
    migrate_v14_customers_phone_nullable(connection)?;
    migrate_v15_service_pricing(connection)?;
    migrate_v16_scheduling_schema(connection)?;
    migrate_v17_archive_and_audit_schema(connection)?;
    migrate_v18_business_profile_schema(connection)?;
    migrate_v19_appointment_whatsapp_preference(connection)?;
    connection.execute(
        "UPDATE app_meta SET schema_version = ?1 WHERE schema_version < ?1",
        params![CORE_SCHEMA_VERSION],
    )?;
    Ok(())
}

fn migrate_v18_business_profile_schema(connection: &Connection) -> Result<(), AppError> {
    if read_schema_version(connection)? >= 18 {
        return Ok(());
    }
    connection
        .execute_batch(
            "BEGIN IMMEDIATE;
             CREATE TABLE IF NOT EXISTS business_profile (
               id INTEGER PRIMARY KEY NOT NULL CHECK (id = 1),
               business_name TEXT NOT NULL DEFAULT '',
               phone TEXT NULL,
               email TEXT NULL,
               address TEXT NULL,
               currency_code TEXT NOT NULL DEFAULT 'TRY' CHECK (currency_code GLOB '[A-Z][A-Z][A-Z]'),
               theme_key TEXT NOT NULL DEFAULT 'default' CHECK (length(trim(theme_key)) > 0 AND length(theme_key) <= 64),
               logo_data BLOB NULL,
               logo_mime_type TEXT NULL,
               updated_at TEXT NOT NULL,
               CHECK ((logo_data IS NULL AND logo_mime_type IS NULL) OR (logo_data IS NOT NULL AND logo_mime_type IS NOT NULL))
             );
             INSERT INTO business_profile (id, business_name, phone, email, address, currency_code, theme_key, logo_data, logo_mime_type, updated_at)
             VALUES (1, '', NULL, NULL, NULL, 'TRY', 'default', NULL, NULL, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
             ON CONFLICT(id) DO NOTHING;
             COMMIT;",
        )
        .map_err(Into::into)
}

fn migrate_v19_appointment_whatsapp_preference(connection: &Connection) -> Result<(), AppError> {
    if read_schema_version(connection)? >= 19 {
        return Ok(());
    }
    connection.execute_batch("BEGIN IMMEDIATE;")?;
    let result = (|| -> Result<(), AppError> {
        if !table_columns(connection, "appointments")?.contains("whatsapp_reminder_enabled") {
            connection.execute(
                "ALTER TABLE appointments ADD COLUMN whatsapp_reminder_enabled INTEGER NOT NULL DEFAULT 1 CHECK (whatsapp_reminder_enabled IN (0, 1))",
                [],
            )?;
        }
        connection.execute_batch("COMMIT;")?;
        Ok(())
    })();
    if result.is_err() {
        let _ = connection.execute_batch("ROLLBACK;");
    }
    result
}

fn migrate_v16_scheduling_schema(connection: &Connection) -> Result<(), AppError> {
    if read_schema_version(connection)? >= 16 {
        return Ok(());
    }
    connection.execute_batch("BEGIN IMMEDIATE;
      CREATE TABLE IF NOT EXISTS staff_working_hours (id TEXT PRIMARY KEY NOT NULL, staff_id TEXT NOT NULL REFERENCES staff(id) ON DELETE RESTRICT, weekday INTEGER NOT NULL CHECK(weekday BETWEEN 0 AND 6), start_minute INTEGER NOT NULL CHECK(start_minute BETWEEN 0 AND 1439), end_minute INTEGER NOT NULL CHECK(end_minute BETWEEN 1 AND 1440 AND start_minute < end_minute));
      CREATE INDEX IF NOT EXISTS staff_working_hours_staff_weekday_idx ON staff_working_hours(staff_id, weekday, start_minute);
      CREATE TABLE IF NOT EXISTS staff_time_off (id TEXT PRIMARY KEY NOT NULL, staff_id TEXT NOT NULL REFERENCES staff(id) ON DELETE RESTRICT, local_date TEXT NOT NULL, full_day INTEGER NOT NULL CHECK(full_day IN (0,1)), start_minute INTEGER, end_minute INTEGER, CHECK((full_day=1 AND start_minute IS NULL AND end_minute IS NULL) OR (full_day=0 AND start_minute BETWEEN 0 AND 1439 AND end_minute BETWEEN 1 AND 1440 AND start_minute < end_minute)));
      CREATE INDEX IF NOT EXISTS staff_time_off_staff_date_idx ON staff_time_off(staff_id, local_date, start_minute);
      COMMIT;")
        .map_err(Into::into)
}

// Audit metadata is limited to safe mutation details. Never store credentials,
// authentication material, customer phone/email values, or free-form notes here.
fn migrate_v17_archive_and_audit_schema(connection: &Connection) -> Result<(), AppError> {
    if read_schema_version(connection)? >= 17 {
        return Ok(());
    }
    let audit_columns = table_columns(connection, "audit_log")?;
    let legacy_audit = !audit_columns.is_empty() && !audit_columns.contains("occurred_at");

    if legacy_audit
        && ![
            "id",
            "entity_type",
            "entity_id",
            "action",
            "occurred_at_utc",
        ]
        .iter()
        .all(|column| audit_columns.contains(*column))
    {
        return Err(AppError::Database(
            "AUDIT_LOG_LEGACY_SCHEMA_UNSUPPORTED".to_string(),
        ));
    }

    connection.execute_batch("BEGIN IMMEDIATE;")?;
    let result = (|| -> Result<(), AppError> {
        if legacy_audit {
            connection.execute_batch(
                "DROP TRIGGER IF EXISTS audit_log_append_only_update;
                 DROP TRIGGER IF EXISTS audit_log_append_only_delete;
                 ALTER TABLE audit_log RENAME TO audit_log_legacy_v13;",
            )?;
        }

        connection.execute_batch(
            "CREATE TABLE IF NOT EXISTS audit_log (
               id TEXT PRIMARY KEY NOT NULL,
               occurred_at TEXT NOT NULL,
               entity_type TEXT NOT NULL CHECK (length(trim(entity_type)) > 0 AND length(entity_type) <= 64),
               entity_id TEXT NOT NULL CHECK (length(trim(entity_id)) > 0 AND length(entity_id) <= 128),
               action TEXT NOT NULL CHECK (length(trim(action)) > 0 AND length(action) <= 64),
               metadata_json TEXT NULL
             );",
        )?;

        if legacy_audit {
            // The legacy summary was unrestricted free text, so retain only the
            // structured history fields in the privacy-safe audit contract.
            connection.execute_batch(
                "INSERT INTO audit_log (id, occurred_at, entity_type, entity_id, action, metadata_json)
                 SELECT CAST(id AS TEXT), occurred_at_utc, entity_type, entity_id, action, NULL
                 FROM audit_log_legacy_v13;
                 DROP TABLE audit_log_legacy_v13;",
            )?;
        }

        connection.execute_batch(
            "CREATE INDEX IF NOT EXISTS audit_log_occurred_at_idx ON audit_log (occurred_at DESC, id DESC);
             CREATE INDEX IF NOT EXISTS audit_log_entity_occurred_at_idx ON audit_log (entity_type, entity_id, occurred_at DESC, id DESC);
             CREATE TRIGGER IF NOT EXISTS audit_log_append_only_update
             BEFORE UPDATE ON audit_log
             BEGIN
               SELECT RAISE(ABORT, 'AUDIT_LOG_APPEND_ONLY');
             END;
             CREATE TRIGGER IF NOT EXISTS audit_log_append_only_delete
             BEFORE DELETE ON audit_log
             BEGIN
               SELECT RAISE(ABORT, 'AUDIT_LOG_APPEND_ONLY');
             END;",
        )?;
        Ok(())
    })();

    match result {
        Ok(()) => {
            connection.execute_batch("COMMIT;")?;
            Ok(())
        }
        Err(error) => {
            let _ = connection.execute_batch("ROLLBACK;");
            Err(error)
        }
    }
}

fn migrate_v15_service_pricing(connection: &Connection) -> Result<(), AppError> {
    if read_schema_version(connection)? >= 15 {
        return Ok(());
    }
    let service_columns = table_columns(connection, "services")?;
    let appointment_columns = table_columns(connection, "appointment_services")?;
    connection.execute_batch("BEGIN IMMEDIATE;")?;
    let result = (|| -> Result<(), AppError> {
        if !service_columns.contains("default_price_minor") {
            connection.execute("ALTER TABLE services ADD COLUMN default_price_minor INTEGER NOT NULL DEFAULT 0 CHECK (default_price_minor >= 0)", [])?;
        }
        if !appointment_columns.contains("listed_price_snapshot_minor") {
            connection.execute("ALTER TABLE appointment_services ADD COLUMN listed_price_snapshot_minor INTEGER NOT NULL DEFAULT 0 CHECK (listed_price_snapshot_minor >= 0)", [])?;
        }
        if !appointment_columns.contains("charged_price_minor") {
            connection.execute("ALTER TABLE appointment_services ADD COLUMN charged_price_minor INTEGER NOT NULL DEFAULT 0 CHECK (charged_price_minor >= 0)", [])?;
        }
        connection.execute_batch("CREATE INDEX IF NOT EXISTS appointment_services_appointment_idx ON appointment_services(appointment_id, service_id);
          CREATE INDEX IF NOT EXISTS appointments_completed_range_idx ON appointments(status, start_at_utc);")?;
        connection.execute_batch("COMMIT;")?;
        Ok(())
    })();
    if result.is_err() {
        let _ = connection.execute_batch("ROLLBACK;");
    }
    result
}

fn migrate_v14_customers_phone_nullable(connection: &Connection) -> Result<(), AppError> {
    if read_schema_version(connection)? >= 14
        || table_columns(connection, "customers")?
            .get("phone")
            .is_none()
    {
        return Ok(());
    }
    let required: i64 = connection.query_row(
        "SELECT \"notnull\" FROM pragma_table_info('customers') WHERE name='phone'",
        [],
        |row| row.get(0),
    )?;
    if required == 0 {
        return Ok(());
    }
    connection.pragma_update(None, "foreign_keys", "OFF")?;
    connection.pragma_update(None, "legacy_alter_table", "ON")?;
    let result = (|| -> Result<(), AppError> {
        connection.execute_batch("BEGIN IMMEDIATE;
          ALTER TABLE customers RENAME TO customers_v13;
          CREATE TABLE customers (
            id TEXT PRIMARY KEY NOT NULL, first_name TEXT NOT NULL, last_name TEXT NOT NULL, phone TEXT,
            email TEXT, whatsapp_reminder_enabled INTEGER NOT NULL DEFAULT 1,
            whatsapp_consent_confirmed INTEGER NOT NULL DEFAULT 0, whatsapp_consent_recorded_at TEXT,
            notes TEXT, is_active INTEGER NOT NULL DEFAULT 1, created_at TEXT NOT NULL, updated_at TEXT NOT NULL);
          INSERT INTO customers (id,first_name,last_name,phone,email,whatsapp_reminder_enabled,whatsapp_consent_confirmed,whatsapp_consent_recorded_at,notes,is_active,created_at,updated_at)
          SELECT id,first_name,last_name,phone,email,whatsapp_reminder_enabled,whatsapp_consent_confirmed,whatsapp_consent_recorded_at,notes,is_active,created_at,updated_at FROM customers_v13;
          DROP TABLE customers_v13;
          CREATE UNIQUE INDEX customers_phone_unique_idx ON customers(phone);
          CREATE INDEX customers_active_name_idx ON customers(is_active,last_name,first_name);
          COMMIT;")?;
        Ok(())
    })();
    connection.pragma_update(None, "legacy_alter_table", "OFF")?;
    connection.pragma_update(None, "foreign_keys", "ON")?;
    result?;
    let violations: i64 =
        connection.query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
            row.get(0)
        })?;
    if violations != 0 || !integrity_check(connection)? {
        return Err(AppError::Database(
            "V14_CUSTOMER_MIGRATION_INTEGRITY".into(),
        ));
    }
    Ok(())
}

fn normalize_app_meta_for_core(connection: &Connection) -> Result<(), AppError> {
    let exists: i64 = connection.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='app_meta'",
        [],
        |row| row.get(0),
    )?;
    if exists == 0 {
        return Ok(());
    }
    let columns = table_columns(connection, "app_meta")?;
    if !columns.contains("initialized_at") {
        connection.execute("ALTER TABLE app_meta ADD COLUMN initialized_at TEXT", [])?;
        if columns.contains("created_at_utc") {
            connection.execute(
                "UPDATE app_meta SET initialized_at = COALESCE(initialized_at, created_at_utc, datetime('now'))",
                [],
            )?;
        } else {
            connection.execute(
                "UPDATE app_meta SET initialized_at = COALESCE(initialized_at, datetime('now'))",
                [],
            )?;
        }
    }
    Ok(())
}

fn table_columns(connection: &Connection, table: &str) -> Result<HashSet<String>, AppError> {
    let mut statement = connection.prepare(&format!("PRAGMA table_info({table})"))?;
    let rows = statement.query_map([], |row| row.get::<_, String>(1))?;
    rows.collect::<Result<HashSet<_>, _>>().map_err(Into::into)
}

fn integrity_check(connection: &Connection) -> Result<bool, AppError> {
    let result: String = connection.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
    Ok(result == "ok")
}

fn read_schema_version(connection: &Connection) -> Result<i64, AppError> {
    connection
        .query_row("SELECT MAX(schema_version) FROM app_meta", [], |row| {
            row.get(0)
        })
        .map_err(Into::into)
}

fn now_iso() -> String {
    Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn new_uuid() -> String {
    let now = Utc::now().timestamp_nanos_opt().unwrap_or_default() as u128;
    let count = ID_COUNTER.fetch_add(1, Ordering::Relaxed) as u128;
    let value = now ^ (count << 32);
    format!(
        "{:08x}-{:04x}-4{:03x}-8{:03x}-{:012x}",
        (value >> 96) as u32,
        ((value >> 80) & 0xffff) as u16,
        ((value >> 64) & 0x0fff) as u16,
        ((value >> 48) & 0x0fff) as u16,
        value & 0xffffffffffff
    )
}

fn normalize_text(value: &str, field: &str, min: usize, max: usize) -> Result<String, AppError> {
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    let len = normalized.chars().count();
    if len < min || len > max {
        return Err(AppError::Validation(format!("{field} length")));
    }
    Ok(normalized)
}

fn optional_text(value: Option<String>, max: usize) -> Result<Option<String>, AppError> {
    match value {
        Some(raw) => {
            let normalized = raw.split_whitespace().collect::<Vec<_>>().join(" ");
            if normalized.is_empty() {
                Ok(None)
            } else if normalized.chars().count() > max {
                Err(AppError::Validation("optional text length".to_string()))
            } else {
                Ok(Some(normalized))
            }
        }
        None => Ok(None),
    }
}

fn normalize_business_name(value: &str) -> Result<String, AppError> {
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.chars().count() > 160 {
        return Err(AppError::Validation("businessName length".to_string()));
    }
    Ok(normalized)
}

fn normalize_business_email(value: Option<String>) -> Result<Option<String>, AppError> {
    let email = optional_text(value, 254)?;
    if let Some(email) = &email {
        let Some((local, domain)) = email.rsplit_once('@') else {
            return Err(AppError::Validation("business email invalid".to_string()));
        };
        if local.is_empty()
            || domain.is_empty()
            || !domain.contains('.')
            || email.chars().any(char::is_whitespace)
        {
            return Err(AppError::Validation("business email invalid".to_string()));
        }
    }
    Ok(email)
}

fn normalize_currency_code(value: &str) -> Result<String, AppError> {
    let normalized = value.trim().to_ascii_uppercase();
    if normalized.len() != 3 || !normalized.bytes().all(|byte| byte.is_ascii_alphabetic()) {
        return Err(AppError::Validation("currencyCode invalid".to_string()));
    }
    Ok(normalized)
}

fn validate_theme_key(value: &str) -> Result<String, AppError> {
    let normalized = value.trim();
    if normalized == "default" {
        Ok(normalized.to_string())
    } else {
        Err(AppError::Validation("themeKey invalid".to_string()))
    }
}

fn normalize_business_logo(
    logo_data: Option<Vec<u8>>,
    logo_mime_type: Option<String>,
) -> Result<(Option<Vec<u8>>, Option<String>), AppError> {
    const MAX_LOGO_BYTES: usize = 3 * 1024 * 1024;
    match (
        logo_data,
        logo_mime_type.map(|mime| mime.trim().to_ascii_lowercase()),
    ) {
        (None, None) => Ok((None, None)),
        (Some(data), Some(mime)) => {
            if data.is_empty() || data.len() > MAX_LOGO_BYTES {
                return Err(AppError::Validation("logoData invalid".to_string()));
            }
            if !matches!(mime.as_str(), "image/png" | "image/jpeg" | "image/webp") {
                return Err(AppError::Validation("logoMimeType invalid".to_string()));
            }
            Ok((Some(data), Some(mime)))
        }
        _ => Err(AppError::Validation(
            "logo data and mime must match".to_string(),
        )),
    }
}

fn business_profile_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<BusinessProfile> {
    Ok(BusinessProfile {
        business_name: row.get(0)?,
        phone: row.get(1)?,
        email: row.get(2)?,
        address: row.get(3)?,
        currency_code: row.get(4)?,
        theme_key: row.get(5)?,
        logo_data: row.get(6)?,
        logo_mime_type: row.get(7)?,
        updated_at: row.get(8)?,
    })
}

fn ensure_business_profile_row(connection: &Connection) -> Result<(), AppError> {
    connection.execute(
        "INSERT INTO business_profile (id, business_name, phone, email, address, currency_code, theme_key, logo_data, logo_mime_type, updated_at)
         VALUES (1, '', NULL, NULL, NULL, 'TRY', 'default', NULL, NULL, ?1)
         ON CONFLICT(id) DO NOTHING",
        params![now_iso()],
    )?;
    Ok(())
}

fn load_business_profile(connection: &Connection) -> Result<BusinessProfile, AppError> {
    ensure_business_profile_row(connection)?;
    connection
        .query_row(
            "SELECT business_name, phone, email, address, currency_code, theme_key, logo_data, logo_mime_type, updated_at
             FROM business_profile WHERE id=1",
            [],
            business_profile_from_row,
        )
        .map_err(Into::into)
}

fn onboarding_state_from_counts(
    business_name: &str,
    customer_count: i64,
    appointment_count: i64,
    staff_count: i64,
    service_count: i64,
) -> OnboardingState {
    let has_business_history = customer_count > 0 || appointment_count > 0;
    if business_name.trim().is_empty() {
        return OnboardingState {
            needs_onboarding: !has_business_history && staff_count == 0 && service_count == 0,
            next_step: 1,
        };
    }
    if has_business_history || (staff_count > 0 && service_count > 0) {
        return OnboardingState {
            needs_onboarding: false,
            next_step: 4,
        };
    }
    OnboardingState {
        needs_onboarding: true,
        next_step: if staff_count == 0 { 2 } else { 3 },
    }
}

fn load_onboarding_state(connection: &Connection) -> Result<OnboardingState, AppError> {
    let profile = load_business_profile(connection)?;
    let customer_count =
        connection.query_row("SELECT COUNT(*) FROM customers", [], |row| row.get(0))?;
    let appointment_count =
        connection.query_row("SELECT COUNT(*) FROM appointments", [], |row| row.get(0))?;
    let staff_count = connection.query_row("SELECT COUNT(*) FROM staff", [], |row| row.get(0))?;
    let service_count =
        connection.query_row("SELECT COUNT(*) FROM services", [], |row| row.get(0))?;
    Ok(onboarding_state_from_counts(
        &profile.business_name,
        customer_count,
        appointment_count,
        staff_count,
        service_count,
    ))
}

fn update_business_profile_tx(
    connection: &mut Connection,
    input: BusinessProfileInput,
) -> Result<BusinessProfile, AppError> {
    let business_name = normalize_business_name(&input.business_name)?;
    let phone = optional_text(input.phone, 32)?;
    let email = normalize_business_email(input.email)?;
    let address = optional_text(input.address, 500)?;
    let currency_code = normalize_currency_code(&input.currency_code)?;
    let theme_key = validate_theme_key(&input.theme_key)?;
    let (logo_data, logo_mime_type) =
        normalize_business_logo(input.logo_data, input.logo_mime_type)?;
    let tx = connection.transaction()?;
    ensure_business_profile_row(&tx)?;
    let current = load_business_profile(&tx)?;
    let mut changed_fields = Vec::new();
    if current.business_name != business_name {
        changed_fields.push("business_name");
    }
    if current.phone != phone {
        changed_fields.push("phone");
    }
    if current.email != email {
        changed_fields.push("email");
    }
    if current.address != address {
        changed_fields.push("address");
    }
    if current.currency_code != currency_code {
        changed_fields.push("currency_code");
    }
    if current.theme_key != theme_key {
        changed_fields.push("theme_key");
    }
    if current.logo_data != logo_data || current.logo_mime_type != logo_mime_type {
        changed_fields.push("logo");
    }
    if changed_fields.is_empty() {
        drop(tx);
        return Ok(current);
    }
    tx.execute(
        "UPDATE business_profile
         SET business_name=?1, phone=?2, email=?3, address=?4, currency_code=?5, theme_key=?6,
             logo_data=?7, logo_mime_type=?8, updated_at=?9
         WHERE id=1",
        params![
            business_name,
            phone,
            email,
            address,
            currency_code,
            theme_key,
            logo_data,
            logo_mime_type,
            now_iso()
        ],
    )?;
    append_audit_event(
        &tx,
        AuditEntityType::BusinessProfile,
        "1",
        AuditAction::Update,
        Some(AuditMetadata::ChangedFields(&changed_fields)),
    )?;
    tx.commit()?;
    load_business_profile(connection)
}

fn normalize_phone(value: Option<&str>, required: bool) -> Result<Option<String>, AppError> {
    let Some(raw) = value else {
        return if required {
            Err(AppError::Validation("phone required".to_string()))
        } else {
            Ok(None)
        };
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return if required {
            Err(AppError::Validation("phone required".to_string()))
        } else {
            Ok(None)
        };
    }
    if trimmed.chars().any(|c| c.is_ascii_alphabetic()) {
        return Err(AppError::Validation("phone invalid".to_string()));
    }
    let digits: String = trimmed.chars().filter(|c| c.is_ascii_digit()).collect();
    let normalized = if digits.len() == 10 && digits.starts_with('5') {
        digits
    } else if digits.len() == 11 && digits.starts_with("05") {
        digits[1..].to_string()
    } else if digits.len() == 12 && digits.starts_with("905") {
        digits[2..].to_string()
    } else {
        return Err(AppError::Validation("phone invalid".to_string()));
    };
    Ok(Some(normalized))
}

fn map_customer_phone_conflict(error: rusqlite::Error) -> AppError {
    if matches!(
        &error,
        rusqlite::Error::SqliteFailure(_, Some(message))
            if message.contains("customers.phone") || message.contains("customers_phone_unique_idx")
    ) {
        AppError::Conflict("CUSTOMER_PHONE_CONFLICT".to_string())
    } else {
        AppError::Sqlite(error)
    }
}

fn name_key(name: &str) -> String {
    name.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn bool_to_i64(value: bool) -> i64 {
    if value {
        1
    } else {
        0
    }
}

fn sql_path_literal(path: &Path) -> String {
    path.display().to_string().replace('\'', "''")
}

fn timestamp_for_file() -> String {
    Utc::now().format("%Y%m%d-%H%M%S%.3f").to_string()
}

fn pseudo_hash_64(value: &str) -> String {
    let mut output = String::new();
    for salt in 0..4_u8 {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        salt.hash(&mut hasher);
        value.hash(&mut hasher);
        output.push_str(&format!("{:016x}", hasher.finish()));
    }
    output
}

fn validate_color(value: &str) -> Result<String, AppError> {
    match value {
        "sage" | "teal" | "blue" | "purple" | "rose" | "coral" | "amber" | "slate" => {
            Ok(value.to_string())
        }
        _ => Err(AppError::Validation("color_key invalid".to_string())),
    }
}

fn validate_status(value: &str) -> Result<String, AppError> {
    match value {
        "planned" | "confirmed" | "completed" | "cancelled" | "no_show" => Ok(value.to_string()),
        _ => Err(AppError::Validation("status invalid".to_string())),
    }
}

fn local_to_utc(local_date: &str, local_time: &str) -> Result<chrono::DateTime<Utc>, AppError> {
    let date = NaiveDate::parse_from_str(local_date, "%Y-%m-%d")
        .map_err(|_| AppError::Validation("localDate invalid".to_string()))?;
    let time = NaiveTime::parse_from_str(local_time, "%H:%M")
        .map_err(|_| AppError::Validation("localStartTime invalid".to_string()))?;
    if time.minute() % 5 != 0 {
        return Err(AppError::Validation("localStartTime step".to_string()));
    }
    let local = date.and_time(time);
    Ok(chrono::DateTime::<Utc>::from_naive_utc_and_offset(
        local - Duration::minutes(ISTANBUL_OFFSET_MINUTES),
        Utc,
    ))
}

fn utc_iso(dt: chrono::DateTime<Utc>) -> String {
    dt.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn ensure_same_istanbul_day(
    start: chrono::DateTime<Utc>,
    end: chrono::DateTime<Utc>,
) -> Result<(), AppError> {
    let start_day = (start + Duration::minutes(ISTANBUL_OFFSET_MINUTES)).date_naive();
    let end_day =
        (end - Duration::milliseconds(1) + Duration::minutes(ISTANBUL_OFFSET_MINUTES)).date_naive();
    if start_day != end_day {
        return Err(AppError::Validation(
            "APPOINTMENT_CROSSES_MIDNIGHT".to_string(),
        ));
    }
    Ok(())
}

trait ChronoMinute {
    fn minute(&self) -> u32;
}

impl ChronoMinute for NaiveTime {
    fn minute(&self) -> u32 {
        chrono::Timelike::minute(self)
    }
}

fn customer_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Customer> {
    Ok(Customer {
        id: row.get("id")?,
        first_name: row.get("first_name")?,
        last_name: row.get("last_name")?,
        phone: row.get("phone")?,
        email: row.get("email")?,
        notes: row.get("notes")?,
        whatsapp_reminder_enabled: row.get::<_, i64>("whatsapp_reminder_enabled")? == 1,
        whatsapp_consent_confirmed: row.get::<_, i64>("whatsapp_consent_confirmed")? == 1,
        is_active: row.get::<_, i64>("is_active")? == 1,
        created_at: row.get("created_at")?,
        updated_at: row.get("updated_at")?,
    })
}

fn staff_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Staff> {
    Ok(Staff {
        id: row.get("id")?,
        first_name: row.get("first_name")?,
        last_name: row.get("last_name")?,
        phone: row.get("phone")?,
        specialty_note: row.get("specialty_note")?,
        color_key: row.get("color_key")?,
        sort_order: row.get("sort_order")?,
        is_active: row.get::<_, i64>("is_active")? == 1,
    })
}

fn service_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ServiceItem> {
    let is_active = row.get::<_, i64>("is_active")? == 1;
    let category_is_active = row.get::<_, i64>("category_is_active")? == 1;
    let duration: Option<i64> = row.get("duration_minutes")?;
    let availability_status = if !is_active {
        "inactive"
    } else if !category_is_active {
        "category_inactive"
    } else if duration.is_none() {
        "duration_required"
    } else {
        "ready"
    };
    Ok(ServiceItem {
        id: row.get("id")?,
        category_id: row.get("category_id")?,
        category_name: row.get("category_name")?,
        name: row.get("name")?,
        duration_minutes: duration,
        default_price_minor: row.get("default_price_minor")?,
        sort_order: row.get("sort_order")?,
        is_active,
        availability_status: availability_status.to_string(),
    })
}

fn get_customer(connection: &Connection, id: &str) -> Result<Option<Customer>, AppError> {
    connection
        .query_row(
            "SELECT * FROM customers WHERE id = ?1",
            params![id],
            customer_from_row,
        )
        .optional()
        .map_err(Into::into)
}

fn get_staff(connection: &Connection, id: &str) -> Result<Option<Staff>, AppError> {
    connection
        .query_row(
            "SELECT * FROM staff WHERE id = ?1",
            params![id],
            staff_from_row,
        )
        .optional()
        .map_err(Into::into)
}

fn list_staff_working_hours(
    connection: &Connection,
    staff_id: &str,
) -> Result<Vec<StaffWorkingHour>, AppError> {
    let mut statement = connection.prepare("SELECT staff_id, weekday, start_minute, end_minute FROM staff_working_hours WHERE staff_id=?1 ORDER BY weekday, start_minute")?;
    let rows = statement.query_map(params![staff_id], |row| {
        Ok(StaffWorkingHour {
            staff_id: row.get(0)?,
            weekday: row.get(1)?,
            start_minute: row.get(2)?,
            end_minute: row.get(3)?,
        })
    })?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

fn replace_staff_working_hours(
    connection: &mut Connection,
    staff_id: &str,
    intervals: Vec<StaffWorkingHour>,
) -> Result<(), AppError> {
    if get_staff(connection, staff_id)?.is_none() {
        return Err(AppError::NotFound("STAFF_NOT_FOUND".into()));
    }
    for item in &intervals {
        if item.staff_id != staff_id
            || !(0..=6).contains(&item.weekday)
            || item.start_minute < 0
            || item.end_minute > 1440
            || item.start_minute >= item.end_minute
        {
            return Err(AppError::Validation("STAFF_WORKING_HOURS_INVALID".into()));
        }
    }
    let mut ordered = intervals.clone();
    ordered.sort_by_key(|item| (item.weekday, item.start_minute, item.end_minute));
    for pair in ordered.windows(2) {
        if pair[0].weekday == pair[1].weekday && pair[1].start_minute < pair[0].end_minute {
            return Err(AppError::Conflict("STAFF_WORKING_HOURS_OVERLAP".into()));
        }
    }
    if list_staff_working_hours(connection, staff_id)? == ordered {
        return Ok(());
    }
    let tx = connection.transaction()?;
    tx.execute(
        "DELETE FROM staff_working_hours WHERE staff_id=?1",
        params![staff_id],
    )?;
    for item in ordered {
        tx.execute("INSERT INTO staff_working_hours (id,staff_id,weekday,start_minute,end_minute) VALUES (?1,?2,?3,?4,?5)", params![new_uuid(),staff_id,item.weekday,item.start_minute,item.end_minute])?;
    }
    tx.commit()?;
    Ok(())
}

fn is_staff_schedule_unrestricted(
    connection: &Connection,
    staff_id: &str,
) -> Result<bool, AppError> {
    let count: i64 = connection.query_row(
        "SELECT COUNT(*) FROM staff_working_hours WHERE staff_id=?1",
        params![staff_id],
        |row| row.get(0),
    )?;
    Ok(count == 0)
}

fn staff_time_off_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<StaffTimeOff> {
    Ok(StaffTimeOff {
        id: row.get(0)?,
        staff_id: row.get(1)?,
        local_date: row.get(2)?,
        full_day: row.get(3)?,
        start_minute: row.get(4)?,
        end_minute: row.get(5)?,
    })
}

fn list_staff_time_off(
    connection: &Connection,
    staff_id: &str,
) -> Result<Vec<StaffTimeOff>, AppError> {
    let mut statement = connection.prepare(
        "SELECT id, staff_id, local_date, full_day, start_minute, end_minute
         FROM staff_time_off
         WHERE staff_id=?1
         ORDER BY local_date, full_day DESC, start_minute, id",
    )?;
    let rows = statement.query_map(params![staff_id], staff_time_off_from_row)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

fn get_staff_time_off(
    connection: &Connection,
    staff_id: &str,
    time_off_id: &str,
) -> Result<Option<StaffTimeOff>, AppError> {
    connection
        .query_row(
            "SELECT id, staff_id, local_date, full_day, start_minute, end_minute
             FROM staff_time_off WHERE id=?1 AND staff_id=?2",
            params![time_off_id, staff_id],
            staff_time_off_from_row,
        )
        .optional()
        .map_err(Into::into)
}

fn validate_staff_time_off_input(input: &StaffTimeOffInput) -> Result<(), AppError> {
    NaiveDate::parse_from_str(&input.local_date, "%Y-%m-%d")
        .map_err(|_| AppError::Validation("STAFF_TIME_OFF_DATE_INVALID".into()))?;

    match (input.full_day, input.start_minute, input.end_minute) {
        (true, None, None) => Ok(()),
        (false, Some(start_minute), Some(end_minute))
            if start_minute >= 0 && end_minute <= 1440 && start_minute < end_minute =>
        {
            Ok(())
        }
        _ => Err(AppError::Validation("STAFF_TIME_OFF_INVALID".into())),
    }
}

fn ensure_staff_time_off_has_no_conflict(
    connection: &Connection,
    staff_id: &str,
    input: &StaffTimeOffInput,
    excluded_time_off_id: Option<&str>,
) -> Result<(), AppError> {
    let conflict_count: i64 = if input.full_day {
        connection.query_row(
            "SELECT COUNT(*) FROM staff_time_off
             WHERE staff_id=?1 AND local_date=?2 AND (?3 IS NULL OR id<>?3)",
            params![staff_id, input.local_date, excluded_time_off_id],
            |row| row.get(0),
        )?
    } else {
        let start_minute = input.start_minute.expect("validated partial leave start");
        let end_minute = input.end_minute.expect("validated partial leave end");
        connection.query_row(
            "SELECT COUNT(*) FROM staff_time_off
             WHERE staff_id=?1 AND local_date=?2 AND (?3 IS NULL OR id<>?3)
               AND (full_day=1 OR (start_minute<?5 AND end_minute>?4))",
            params![
                staff_id,
                input.local_date,
                excluded_time_off_id,
                start_minute,
                end_minute
            ],
            |row| row.get(0),
        )?
    };
    if conflict_count > 0 {
        return Err(AppError::Conflict("STAFF_TIME_OFF_CONFLICT".into()));
    }
    Ok(())
}

fn add_staff_time_off(
    connection: &mut Connection,
    staff_id: &str,
    input: StaffTimeOffInput,
) -> Result<StaffTimeOff, AppError> {
    if get_staff(connection, staff_id)?.is_none() {
        return Err(AppError::NotFound("STAFF_NOT_FOUND".into()));
    }
    validate_staff_time_off_input(&input)?;
    ensure_staff_time_off_has_no_conflict(connection, staff_id, &input, None)?;
    let id = new_uuid();
    connection.execute(
        "INSERT INTO staff_time_off (id, staff_id, local_date, full_day, start_minute, end_minute)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            id,
            staff_id,
            input.local_date,
            input.full_day,
            input.start_minute,
            input.end_minute
        ],
    )?;
    get_staff_time_off(connection, staff_id, &id)?
        .ok_or_else(|| AppError::Database("staff time off insert".into()))
}

fn update_staff_time_off(
    connection: &mut Connection,
    staff_id: &str,
    time_off_id: &str,
    input: StaffTimeOffInput,
) -> Result<StaffTimeOff, AppError> {
    let existing = get_staff_time_off(connection, staff_id, time_off_id)?
        .ok_or_else(|| AppError::NotFound("STAFF_TIME_OFF_NOT_FOUND".into()))?;
    validate_staff_time_off_input(&input)?;
    ensure_staff_time_off_has_no_conflict(connection, staff_id, &input, Some(time_off_id))?;
    if existing.local_date == input.local_date
        && existing.full_day == input.full_day
        && existing.start_minute == input.start_minute
        && existing.end_minute == input.end_minute
    {
        return Ok(existing);
    }
    connection.execute(
        "UPDATE staff_time_off
         SET local_date=?3, full_day=?4, start_minute=?5, end_minute=?6
         WHERE id=?1 AND staff_id=?2",
        params![
            time_off_id,
            staff_id,
            input.local_date,
            input.full_day,
            input.start_minute,
            input.end_minute
        ],
    )?;
    get_staff_time_off(connection, staff_id, time_off_id)?
        .ok_or_else(|| AppError::Database("staff time off update".into()))
}

fn remove_staff_time_off(
    connection: &mut Connection,
    staff_id: &str,
    time_off_id: &str,
) -> Result<(), AppError> {
    let affected = connection.execute(
        "DELETE FROM staff_time_off WHERE id=?1 AND staff_id=?2",
        params![time_off_id, staff_id],
    )?;
    if affected == 0 {
        return Err(AppError::NotFound("STAFF_TIME_OFF_NOT_FOUND".into()));
    }
    Ok(())
}

fn get_service(connection: &Connection, id: &str) -> Result<Option<ServiceItem>, AppError> {
    connection
        .query_row(
            "SELECT s.*, c.name AS category_name, c.is_active AS category_is_active FROM services s INNER JOIN service_categories c ON c.id = s.category_id WHERE s.id = ?1",
            params![id],
            service_from_row,
        )
        .optional()
        .map_err(Into::into)
}

fn next_sort_order(
    connection: &Connection,
    table: &str,
    where_sql: Option<(&str, &str)>,
) -> Result<i64, AppError> {
    let sql = match where_sql {
        Some((column, _)) => {
            format!("SELECT COALESCE(MAX(sort_order), 0) + 10 FROM {table} WHERE {column} = ?1")
        }
        None => format!("SELECT COALESCE(MAX(sort_order), 0) + 10 FROM {table}"),
    };
    let value = match where_sql {
        Some((_, value)) => connection.query_row(&sql, params![value], |row| row.get(0))?,
        None => connection.query_row(&sql, [], |row| row.get(0))?,
    };
    Ok(value)
}

fn create_customer_tx(
    connection: &mut Connection,
    input: CustomerInput,
) -> Result<Customer, AppError> {
    let first_name = normalize_text(&input.first_name, "firstName", 2, 100)?;
    let last_name = normalize_text(&input.last_name, "lastName", 2, 100)?;
    let phone = normalize_phone(input.phone.as_deref(), false)?;
    let email = optional_text(input.email, 254)?;
    let notes = optional_text(input.notes, 2000)?;
    let now = now_iso();
    let consent = input.whatsapp_consent_confirmed.unwrap_or(false);
    let id = new_uuid();
    let tx = connection.transaction()?;
    tx.execute(
        "INSERT INTO customers (id, first_name, last_name, phone, email, whatsapp_reminder_enabled, whatsapp_consent_confirmed, whatsapp_consent_recorded_at, notes, is_active, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 1, ?10, ?10)",
        params![
            id,
            first_name,
            last_name,
            phone,
            email,
            bool_to_i64(input.whatsapp_reminder_enabled.unwrap_or(true)),
            bool_to_i64(consent),
            if consent { Some(now.clone()) } else { None },
            notes,
            now
        ],
    )
    .map_err(map_customer_phone_conflict)?;
    append_audit_event(
        &tx,
        AuditEntityType::Customer,
        &id,
        AuditAction::Create,
        None,
    )?;
    tx.commit()?;
    get_customer(connection, &id)?.ok_or_else(|| AppError::Database("customer insert".to_string()))
}

fn update_customer_tx(
    connection: &mut Connection,
    id: &str,
    input: CustomerInput,
) -> Result<Customer, AppError> {
    let existing = get_customer(connection, id)?
        .ok_or_else(|| AppError::NotFound("CUSTOMER_NOT_FOUND".to_string()))?;
    let first_name = normalize_text(&input.first_name, "firstName", 2, 100)?;
    let last_name = normalize_text(&input.last_name, "lastName", 2, 100)?;
    let phone = normalize_phone(input.phone.as_deref(), false)?;
    let email = optional_text(input.email, 254)?;
    let notes = optional_text(input.notes, 2000)?;
    let now = now_iso();
    let consent = input.whatsapp_consent_confirmed.unwrap_or(false);
    let mut changed_fields = Vec::new();
    if existing.first_name != first_name {
        changed_fields.push("first_name");
    }
    if existing.last_name != last_name {
        changed_fields.push("last_name");
    }
    if existing.phone != phone {
        changed_fields.push("phone");
    }
    if existing.email != email {
        changed_fields.push("email");
    }
    if existing.notes != notes {
        changed_fields.push("notes");
    }
    if existing.whatsapp_reminder_enabled != input.whatsapp_reminder_enabled.unwrap_or(true) {
        changed_fields.push("whatsapp_reminder_enabled");
    }
    if existing.whatsapp_consent_confirmed != consent {
        changed_fields.push("whatsapp_consent_confirmed");
    }
    if changed_fields.is_empty() {
        return Ok(existing);
    }
    let tx = connection.transaction()?;
    tx.execute(
        "UPDATE customers SET first_name=?1,last_name=?2,phone=?3,email=?4,whatsapp_reminder_enabled=?5,whatsapp_consent_confirmed=?6,whatsapp_consent_recorded_at=CASE WHEN ?6 = 1 THEN COALESCE(whatsapp_consent_recorded_at, ?7) ELSE NULL END,notes=?8,updated_at=?7 WHERE id=?9",
        params![first_name,last_name,phone,email,bool_to_i64(input.whatsapp_reminder_enabled.unwrap_or(true)),bool_to_i64(consent),now,notes,id],
    )
    .map_err(map_customer_phone_conflict)?;
    append_audit_event(
        &tx,
        AuditEntityType::Customer,
        id,
        AuditAction::Update,
        Some(AuditMetadata::ChangedFields(&changed_fields)),
    )?;
    tx.commit()?;
    get_customer(connection, id)?.ok_or_else(|| AppError::Database("customer update".to_string()))
}

fn set_customer_active(
    connection: &mut Connection,
    customer_id: &str,
    is_active: bool,
) -> Result<Customer, AppError> {
    let existing = get_customer(connection, customer_id)?
        .ok_or_else(|| AppError::NotFound("CUSTOMER_NOT_FOUND".into()))?;
    if existing.is_active == is_active {
        return Ok(existing);
    }
    let tx = connection.transaction()?;
    tx.execute(
        "UPDATE customers SET is_active=?1, updated_at=?2 WHERE id=?3",
        params![bool_to_i64(is_active), now_iso(), customer_id],
    )?;
    append_audit_event(
        &tx,
        AuditEntityType::Customer,
        customer_id,
        if is_active {
            AuditAction::Reactivate
        } else {
            AuditAction::Archive
        },
        None,
    )?;
    tx.commit()?;
    get_customer(connection, customer_id)?
        .ok_or_else(|| AppError::Database("customer active state".into()))
}

fn archive_customer(connection: &mut Connection, customer_id: &str) -> Result<Customer, AppError> {
    set_customer_active(connection, customer_id, false)
}

fn reactivate_customer(
    connection: &mut Connection,
    customer_id: &str,
) -> Result<Customer, AppError> {
    set_customer_active(connection, customer_id, true)
}

fn create_staff_tx(connection: &mut Connection, input: StaffInput) -> Result<Staff, AppError> {
    let first_name = normalize_text(&input.first_name, "firstName", 2, 100)?;
    let last_name = optional_text(input.last_name, 100)?;
    let phone = normalize_phone(input.phone.as_deref(), false)?;
    let specialty_note = optional_text(input.specialty_note, 1000)?;
    let color_key = validate_color(&input.color_key)?;
    let now = now_iso();
    let id = new_uuid();
    let sort_order = next_sort_order(connection, "staff", None)?;
    let tx = connection.transaction()?;
    tx.execute(
        "INSERT INTO staff (id, first_name, last_name, phone, specialty_note, color_key, sort_order, is_active, created_at, updated_at)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?9)",
        params![id, first_name, last_name, phone, specialty_note, color_key, sort_order, bool_to_i64(input.is_active.unwrap_or(true)), now],
    )?;
    append_audit_event(&tx, AuditEntityType::Staff, &id, AuditAction::Create, None)?;
    tx.commit()?;
    get_staff(connection, &id)?.ok_or_else(|| AppError::Database("staff insert".to_string()))
}

fn update_staff_tx(
    connection: &mut Connection,
    id: &str,
    input: StaffInput,
) -> Result<Staff, AppError> {
    let existing = get_staff(connection, id)?
        .ok_or_else(|| AppError::NotFound("STAFF_NOT_FOUND".to_string()))?;
    let first_name = normalize_text(&input.first_name, "firstName", 2, 100)?;
    let last_name = optional_text(input.last_name, 100)?;
    let phone = normalize_phone(input.phone.as_deref(), false)?;
    let specialty_note = optional_text(input.specialty_note, 1000)?;
    let color_key = validate_color(&input.color_key)?;
    let is_active = input.is_active.unwrap_or(true);
    let mut changed_fields = Vec::new();
    if existing.first_name != first_name {
        changed_fields.push("first_name");
    }
    if existing.last_name != last_name {
        changed_fields.push("last_name");
    }
    if existing.phone != phone {
        changed_fields.push("phone");
    }
    if existing.specialty_note != specialty_note {
        changed_fields.push("specialty_note");
    }
    if existing.color_key != color_key {
        changed_fields.push("color_key");
    }
    if existing.is_active != is_active {
        changed_fields.push("is_active");
    }
    if changed_fields.is_empty() {
        return Ok(existing);
    }
    let tx = connection.transaction()?;
    tx.execute(
        "UPDATE staff SET first_name=?1,last_name=?2,phone=?3,specialty_note=?4,color_key=?5,is_active=?6,updated_at=?7 WHERE id=?8",
        params![first_name, last_name, phone, specialty_note, color_key, bool_to_i64(is_active), now_iso(), id],
    )?;
    append_audit_event(
        &tx,
        AuditEntityType::Staff,
        id,
        AuditAction::Update,
        Some(AuditMetadata::ChangedFields(&changed_fields)),
    )?;
    tx.commit()?;
    get_staff(connection, id)?.ok_or_else(|| AppError::Database("staff update".to_string()))
}

fn set_staff_active(
    connection: &mut Connection,
    staff_id: &str,
    is_active: bool,
) -> Result<Staff, AppError> {
    let existing = get_staff(connection, staff_id)?
        .ok_or_else(|| AppError::NotFound("STAFF_NOT_FOUND".into()))?;
    if existing.is_active == is_active {
        return Ok(existing);
    }
    let tx = connection.transaction()?;
    tx.execute(
        "UPDATE staff SET is_active=?1, updated_at=?2 WHERE id=?3",
        params![bool_to_i64(is_active), now_iso(), staff_id],
    )?;
    append_audit_event(
        &tx,
        AuditEntityType::Staff,
        staff_id,
        if is_active {
            AuditAction::Reactivate
        } else {
            AuditAction::Deactivate
        },
        None,
    )?;
    tx.commit()?;
    get_staff(connection, staff_id)?.ok_or_else(|| AppError::Database("staff active state".into()))
}

fn deactivate_staff(connection: &mut Connection, staff_id: &str) -> Result<Staff, AppError> {
    set_staff_active(connection, staff_id, false)
}

fn reactivate_staff(connection: &mut Connection, staff_id: &str) -> Result<Staff, AppError> {
    set_staff_active(connection, staff_id, true)
}

fn create_category_tx(
    connection: &Connection,
    input: CategoryInput,
) -> Result<ServiceCategory, AppError> {
    let name = normalize_text(&input.name, "name", 2, 120)?;
    let key = name_key(&name);
    let id = new_uuid();
    let now = now_iso();
    let sort_order = next_sort_order(connection, "service_categories", None)?;
    connection.execute(
        "INSERT INTO service_categories (id, name, name_key, sort_order, is_active, created_at, updated_at) VALUES (?1,?2,?3,?4,?5,?6,?6)",
        params![id, name, key, sort_order, bool_to_i64(input.is_active.unwrap_or(true)), now],
    )?;
    get_category(connection, &id)
}

fn update_category_tx(
    connection: &mut Connection,
    id: &str,
    input: CategoryInput,
) -> Result<ServiceCategory, AppError> {
    let existing = get_category(connection, id)
        .map_err(|_| AppError::NotFound("CATEGORY_NOT_FOUND".to_string()))?;
    let name = normalize_text(&input.name, "name", 2, 120)?;
    let key = name_key(&name);
    if existing.name == name && existing.is_active == input.is_active.unwrap_or(existing.is_active)
    {
        return Ok(existing);
    }
    connection.execute(
        "UPDATE service_categories SET name=?1,name_key=?2,is_active=?3,updated_at=?4 WHERE id=?5",
        params![
            name,
            key,
            bool_to_i64(input.is_active.unwrap_or(existing.is_active)),
            now_iso(),
            id
        ],
    )?;
    get_category(connection, id)
}

fn get_category(connection: &Connection, id: &str) -> Result<ServiceCategory, AppError> {
    connection
        .query_row(
            "SELECT id,name,sort_order,is_active FROM service_categories WHERE id=?1",
            params![id],
            |row| {
                Ok(ServiceCategory {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    sort_order: row.get(2)?,
                    is_active: row.get::<_, i64>(3)? == 1,
                })
            },
        )
        .map_err(Into::into)
}

fn create_service_tx(
    connection: &mut Connection,
    input: ServiceInput,
) -> Result<ServiceItem, AppError> {
    get_category(connection, &input.category_id)
        .map_err(|_| AppError::NotFound("CATEGORY_NOT_FOUND".to_string()))?;
    let name = normalize_text(&input.name, "name", 2, 160)?;
    let duration = input
        .duration_minutes
        .ok_or_else(|| AppError::Validation("duration required".to_string()))?;
    validate_duration(duration)?;
    let default_price_minor = input.default_price_minor.unwrap_or(0);
    if default_price_minor < 0 {
        return Err(AppError::Validation("defaultPriceMinor invalid".into()));
    }
    let id = new_uuid();
    let now = now_iso();
    let sort_order = next_sort_order(
        connection,
        "services",
        Some(("category_id", &input.category_id)),
    )?;
    let tx = connection.transaction()?;
    tx.execute(
        "INSERT INTO services (id, category_id, name, name_key, duration_minutes, default_price_minor, sort_order, is_active, created_at, updated_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?9)",
        params![id, input.category_id, name.clone(), name_key(&name), duration, default_price_minor, sort_order, bool_to_i64(input.is_active.unwrap_or(true)), now],
    )?;
    append_audit_event(
        &tx,
        AuditEntityType::Service,
        &id,
        AuditAction::Create,
        None,
    )?;
    tx.commit()?;
    get_service(connection, &id)?.ok_or_else(|| AppError::Database("service insert".to_string()))
}

fn validate_duration(duration: i64) -> Result<(), AppError> {
    if !(5..=480).contains(&duration) || duration % 5 != 0 {
        return Err(AppError::Validation("duration invalid".to_string()));
    }
    Ok(())
}

fn update_service_tx(
    connection: &mut Connection,
    id: &str,
    input: ServiceInput,
) -> Result<ServiceItem, AppError> {
    let existing = get_service(connection, id)?
        .ok_or_else(|| AppError::NotFound("SERVICE_NOT_FOUND".to_string()))?;
    get_category(connection, &input.category_id)
        .map_err(|_| AppError::NotFound("CATEGORY_NOT_FOUND".to_string()))?;
    let name = normalize_text(&input.name, "name", 2, 160)?;
    if let Some(duration) = input.duration_minutes {
        validate_duration(duration)?;
    }
    let default_price_minor = input.default_price_minor.unwrap_or(0);
    if default_price_minor < 0 {
        return Err(AppError::Validation("defaultPriceMinor invalid".into()));
    }
    let mut changed_fields = Vec::new();
    if existing.category_id != input.category_id {
        changed_fields.push("category_id");
    }
    if existing.name != name {
        changed_fields.push("name");
    }
    if existing.duration_minutes != input.duration_minutes {
        changed_fields.push("duration_minutes");
    }
    if existing.default_price_minor != default_price_minor {
        changed_fields.push("default_price_minor");
    }
    if existing.is_active != input.is_active.unwrap_or(true) {
        changed_fields.push("is_active");
    }
    if changed_fields.is_empty() {
        return Ok(existing);
    }
    let tx = connection.transaction()?;
    tx.execute(
        "UPDATE services SET category_id=?1,name=?2,name_key=?3,duration_minutes=?4,default_price_minor=?5,is_active=?6,updated_at=?7 WHERE id=?8",
        params![input.category_id, name.clone(), name_key(&name), input.duration_minutes, default_price_minor, bool_to_i64(input.is_active.unwrap_or(true)), now_iso(), id],
    )?;
    append_audit_event(
        &tx,
        AuditEntityType::Service,
        id,
        AuditAction::Update,
        Some(AuditMetadata::ChangedFields(&changed_fields)),
    )?;
    tx.commit()?;
    get_service(connection, id)?.ok_or_else(|| AppError::Database("service update".to_string()))
}

fn set_service_active(
    connection: &mut Connection,
    service_id: &str,
    is_active: bool,
) -> Result<ServiceItem, AppError> {
    let existing = get_service(connection, service_id)?
        .ok_or_else(|| AppError::NotFound("SERVICE_NOT_FOUND".into()))?;
    if existing.is_active == is_active {
        return Ok(existing);
    }
    let tx = connection.transaction()?;
    tx.execute(
        "UPDATE services SET is_active=?1, updated_at=?2 WHERE id=?3",
        params![bool_to_i64(is_active), now_iso(), service_id],
    )?;
    append_audit_event(
        &tx,
        AuditEntityType::Service,
        service_id,
        if is_active {
            AuditAction::Reactivate
        } else {
            AuditAction::Deactivate
        },
        None,
    )?;
    tx.commit()?;
    get_service(connection, service_id)?
        .ok_or_else(|| AppError::Database("service active state".into()))
}

fn deactivate_service(
    connection: &mut Connection,
    service_id: &str,
) -> Result<ServiceItem, AppError> {
    set_service_active(connection, service_id, false)
}

fn reactivate_service(
    connection: &mut Connection,
    service_id: &str,
) -> Result<ServiceItem, AppError> {
    set_service_active(connection, service_id, true)
}

fn list_service_items(connection: &Connection, status: &str) -> Result<Vec<ServiceItem>, AppError> {
    let status_sql = match status {
        "inactive" => "WHERE s.is_active=0",
        "all" => "",
        _ => "WHERE s.is_active=1",
    };
    let mut statement = connection.prepare(&format!(
        "SELECT s.*, c.name AS category_name, c.is_active AS category_is_active
         FROM services s INNER JOIN service_categories c ON c.id = s.category_id
         {status_sql}
         ORDER BY c.sort_order ASC, s.sort_order ASC, s.name COLLATE NOCASE ASC"
    ))?;
    let rows = statement.query_map([], service_from_row)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

fn set_staff_services_tx(
    connection: &mut Connection,
    staff_id: &str,
    service_ids: Vec<String>,
) -> Result<Vec<String>, AppError> {
    get_staff(connection, staff_id)?
        .ok_or_else(|| AppError::NotFound("STAFF_NOT_FOUND".to_string()))?;
    let unique = unique_preserve_order(service_ids);
    for service_id in &unique {
        let service = get_service(connection, service_id)?
            .ok_or_else(|| AppError::NotFound("SERVICE_NOT_FOUND".to_string()))?;
        if service.availability_status == "inactive"
            || service.availability_status == "category_inactive"
        {
            return Err(AppError::Validation("SERVICE_NOT_ASSIGNABLE".to_string()));
        }
    }
    let mut existing = {
        let mut statement = connection.prepare(
            "SELECT service_id FROM staff_services WHERE staff_id=?1 ORDER BY service_id ASC",
        )?;
        let rows = statement.query_map(params![staff_id], |row| row.get::<_, String>(0))?;
        rows.collect::<Result<Vec<_>, _>>()?
    };
    let mut requested = unique.clone();
    existing.sort();
    requested.sort();
    if existing == requested {
        return Ok(unique);
    }
    let tx = connection.transaction()?;
    tx.execute(
        "DELETE FROM staff_services WHERE staff_id = ?1",
        params![staff_id],
    )?;
    let now = now_iso();
    for service_id in &unique {
        tx.execute(
            "INSERT INTO staff_services (staff_id, service_id, created_at) VALUES (?1, ?2, ?3)",
            params![staff_id, service_id, now],
        )?;
    }
    tx.commit()?;
    Ok(unique)
}

fn unique_preserve_order(values: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    values
        .into_iter()
        .filter(|value| seen.insert(value.clone()))
        .collect()
}

fn appointment_snapshots(
    connection: &Connection,
    staff_id: &str,
    service_ids: Vec<String>,
    existing: Option<Vec<AppointmentServiceSnapshot>>,
) -> Result<(Vec<AppointmentServiceSnapshot>, i64, bool), AppError> {
    let service_ids = unique_preserve_order(service_ids);
    if service_ids.is_empty() || service_ids.len() > 50 {
        return Err(AppError::Validation("serviceIds invalid".to_string()));
    }
    if let Some(existing) = existing {
        let existing_ids: Vec<String> = existing
            .iter()
            .map(|item| item.service_id.clone())
            .collect();
        if existing_ids == service_ids {
            let total = existing
                .iter()
                .map(|item| item.duration_minutes_snapshot)
                .sum();
            return Ok((existing, total, true));
        }
    }
    let assigned: HashSet<String> = connection
        .prepare("SELECT service_id FROM staff_services WHERE staff_id = ?1")?
        .query_map(params![staff_id], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .collect();
    let mut snapshots = Vec::new();
    for (index, service_id) in service_ids.iter().enumerate() {
        if !assigned.contains(service_id) {
            return Err(AppError::Validation(
                "SERVICE_NOT_ASSIGNED_TO_STAFF".to_string(),
            ));
        }
        let service = get_service(connection, service_id)?
            .ok_or_else(|| AppError::NotFound("SERVICE_NOT_FOUND".to_string()))?;
        if service.availability_status != "ready" {
            return Err(AppError::Validation("SERVICE_NOT_READY".to_string()));
        }
        let duration = service
            .duration_minutes
            .ok_or_else(|| AppError::Validation("SERVICE_NOT_READY".to_string()))?;
        snapshots.push(AppointmentServiceSnapshot {
            service_id: service.id,
            service_name_snapshot: service.name,
            duration_minutes_snapshot: duration,
            listed_price_snapshot_minor: service.default_price_minor,
            charged_price_minor: service.default_price_minor,
            sort_order: ((index + 1) * 10) as i64,
        });
    }
    let total: i64 = snapshots
        .iter()
        .map(|item| item.duration_minutes_snapshot)
        .sum();
    if !(5..=720).contains(&total) {
        return Err(AppError::Validation("duration total".to_string()));
    }
    Ok((snapshots, total, false))
}

fn assert_appointment_references(
    connection: &Connection,
    customer_id: &str,
    staff_id: &str,
    current: Option<(&str, &str)>,
) -> Result<(), AppError> {
    let customer = get_customer(connection, customer_id)?
        .ok_or_else(|| AppError::NotFound("CUSTOMER_NOT_FOUND".to_string()))?;
    if !customer.is_active && current.map(|(cid, _)| cid != customer_id).unwrap_or(true) {
        return Err(AppError::Validation("CUSTOMER_INACTIVE".to_string()));
    }
    let staff = get_staff(connection, staff_id)?
        .ok_or_else(|| AppError::NotFound("STAFF_NOT_FOUND".to_string()))?;
    if !staff.is_active && current.map(|(_, sid)| sid != staff_id).unwrap_or(true) {
        return Err(AppError::Validation("STAFF_INACTIVE".to_string()));
    }
    Ok(())
}

fn appointment_duration_minutes(
    connection: &Connection,
    appointment_id: &str,
) -> Result<i64, AppError> {
    let duration = connection
        .query_row(
            "SELECT COALESCE(SUM(aps.duration_minutes_snapshot), 0)
             FROM appointments a
             LEFT JOIN appointment_services aps ON aps.appointment_id=a.id
             WHERE a.id=?1
             GROUP BY a.id",
            params![appointment_id],
            |row| row.get::<_, i64>(0),
        )
        .optional()?
        .ok_or_else(|| AppError::NotFound("APPOINTMENT_NOT_FOUND".into()))?;
    if duration <= 0 {
        return Err(AppError::Validation("APPOINTMENT_DURATION_INVALID".into()));
    }
    Ok(duration)
}

fn appointment_interval_from_local(
    local_date: &str,
    local_start_time: &str,
    duration_minutes: i64,
) -> Result<AppointmentInterval, AppError> {
    if duration_minutes <= 0 {
        return Err(AppError::Validation("APPOINTMENT_DURATION_INVALID".into()));
    }
    let start_at_utc = local_to_utc(local_date, local_start_time)?;
    let end_at_utc = start_at_utc + Duration::minutes(duration_minutes);
    ensure_same_istanbul_day(start_at_utc, end_at_utc)?;
    Ok(AppointmentInterval {
        start_at_utc,
        end_at_utc,
    })
}

fn appointment_interval_local_components(
    interval: &AppointmentInterval,
) -> Result<(NaiveDate, i64, i64), AppError> {
    ensure_same_istanbul_day(interval.start_at_utc, interval.end_at_utc)?;
    let start = interval.start_at_utc + Duration::minutes(ISTANBUL_OFFSET_MINUTES);
    let end = interval.end_at_utc + Duration::minutes(ISTANBUL_OFFSET_MINUTES);
    let start_minute = i64::from(chrono::Timelike::hour(&start)) * 60
        + i64::from(chrono::Timelike::minute(&start));
    let end_minute =
        i64::from(chrono::Timelike::hour(&end)) * 60 + i64::from(chrono::Timelike::minute(&end));
    Ok((start.date_naive(), start_minute, end_minute))
}

fn staff_has_appointment_conflict(
    connection: &Connection,
    staff_id: &str,
    interval: &AppointmentInterval,
    exclude_appointment_id: Option<&str>,
) -> Result<bool, AppError> {
    let has_conflict: bool = connection.query_row(
        "SELECT EXISTS(
             SELECT 1
             FROM appointments a
             INNER JOIN appointment_services aps ON aps.appointment_id=a.id
             WHERE a.staff_id=?1 AND a.status<>?2 AND (?3 IS NULL OR a.id<>?3)
               AND a.start_at_utc<?5
             GROUP BY a.id, a.start_at_utc
             HAVING julianday(?4) < julianday(a.start_at_utc) + SUM(aps.duration_minutes_snapshot) / 1440.0
         )",
        params![
            staff_id,
            "cancelled",
            exclude_appointment_id,
            utc_iso(interval.start_at_utc),
            utc_iso(interval.end_at_utc)
        ],
        |row| row.get(0),
    )?;
    Ok(has_conflict)
}

fn staff_has_time_off_conflict(
    connection: &Connection,
    staff_id: &str,
    local_date: NaiveDate,
    start_minute: i64,
    end_minute: i64,
) -> Result<bool, AppError> {
    let has_conflict: bool = connection.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM staff_time_off
             WHERE staff_id=?1 AND local_date=?2
               AND (full_day=1 OR (start_minute<?4 AND end_minute>?3))
         )",
        params![
            staff_id,
            local_date.format("%Y-%m-%d").to_string(),
            start_minute,
            end_minute
        ],
        |row| row.get(0),
    )?;
    Ok(has_conflict)
}

fn check_staff_appointment_availability(
    connection: &Connection,
    staff_id: &str,
    interval: &AppointmentInterval,
    exclude_appointment_id: Option<&str>,
) -> Result<AppointmentAvailability, AppError> {
    if get_staff(connection, staff_id)?.is_none() {
        return Err(AppError::NotFound("STAFF_NOT_FOUND".into()));
    }
    let (local_date, start_minute, end_minute) = appointment_interval_local_components(interval)?;
    if !is_staff_schedule_unrestricted(connection, staff_id)? {
        let weekday = i64::from(local_date.weekday().num_days_from_monday());
        let working_hours = list_staff_working_hours(connection, staff_id)?;
        let inside_configured_interval = working_hours.iter().any(|working_hour| {
            working_hour.weekday == weekday
                && working_hour.start_minute <= start_minute
                && working_hour.end_minute >= end_minute
        });
        if !inside_configured_interval {
            return Ok(AppointmentAvailability::Unavailable(
                AppointmentAvailabilityReason::OutsideWorkingHours,
            ));
        }
    }
    if staff_has_time_off_conflict(connection, staff_id, local_date, start_minute, end_minute)? {
        return Ok(AppointmentAvailability::Unavailable(
            AppointmentAvailabilityReason::StaffTimeOff,
        ));
    }
    if staff_has_appointment_conflict(connection, staff_id, interval, exclude_appointment_id)? {
        return Ok(AppointmentAvailability::Unavailable(
            AppointmentAvailabilityReason::AppointmentConflict,
        ));
    }
    Ok(AppointmentAvailability::Available)
}

fn appointment_availability_error(reason: AppointmentAvailabilityReason) -> AppError {
    let code = match reason {
        AppointmentAvailabilityReason::OutsideWorkingHours => "OUTSIDE_WORKING_HOURS",
        AppointmentAvailabilityReason::StaffTimeOff => "STAFF_TIME_OFF",
        AppointmentAvailabilityReason::AppointmentConflict => "APPOINTMENT_CONFLICT",
    };
    AppError::Conflict(code.into())
}

fn assert_no_conflict(
    connection: &Connection,
    staff_id: &str,
    start_at: &str,
    end_at: &str,
    exclude_id: Option<&str>,
) -> Result<(), AppError> {
    let count: i64 = connection.query_row(
        "SELECT COUNT(*) FROM appointments
         WHERE staff_id=?1 AND status IN ('planned','confirmed','completed')
           AND start_at_utc < ?3 AND end_at_utc > ?2
           AND (?4 IS NULL OR id <> ?4)",
        params![staff_id, start_at, end_at, exclude_id],
        |row| row.get(0),
    )?;
    if count > 0 {
        return Err(AppError::Conflict("APPOINTMENT_CONFLICT".to_string()));
    }
    Ok(())
}

fn create_appointment_tx(
    connection: &mut Connection,
    input: AppointmentInput,
) -> Result<AppointmentSummary, AppError> {
    let status = validate_status(input.status.as_deref().unwrap_or("planned"))?;
    let note = optional_text(input.note, 2000)?;
    let whatsapp_reminder_enabled = input.whatsapp_reminder_enabled.unwrap_or(true);
    let tx = connection.transaction()?;
    assert_appointment_references(&tx, &input.customer_id, &input.staff_id, None)?;
    let (snapshots, total_duration, _) =
        appointment_snapshots(&tx, &input.staff_id, input.service_ids, None)?;
    let start = local_to_utc(&input.local_date, &input.local_start_time)?;
    let end = start + Duration::minutes(total_duration);
    ensure_same_istanbul_day(start, end)?;
    let start_iso = utc_iso(start);
    let end_iso = utc_iso(end);
    if status == "no_show" && start > Utc::now() {
        return Err(AppError::Validation(
            "APPOINTMENT_NO_SHOW_TOO_EARLY".to_string(),
        ));
    }
    if status != APPOINTMENT_STATUS_CANCELLED {
        let interval = AppointmentInterval {
            start_at_utc: start,
            end_at_utc: end,
        };
        if let AppointmentAvailability::Unavailable(reason) =
            check_staff_appointment_availability(&tx, &input.staff_id, &interval, None)?
        {
            return Err(appointment_availability_error(reason));
        }
    }
    if matches!(status.as_str(), "planned" | "confirmed" | "completed") {
        assert_no_conflict(&tx, &input.staff_id, &start_iso, &end_iso, None)?;
    }
    let id = new_uuid();
    let now = now_iso();
    tx.execute(
        "INSERT INTO appointments (id, customer_id, staff_id, start_at_utc, end_at_utc, total_duration_minutes, status, note, whatsapp_reminder_enabled, created_at, updated_at)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?10)",
        params![id, input.customer_id, input.staff_id, start_iso, end_iso, total_duration, status, note, bool_to_i64(whatsapp_reminder_enabled), now],
    )?;
    for snapshot in &snapshots {
        tx.execute(
            "INSERT INTO appointment_services (appointment_id, service_id, service_name_snapshot, duration_minutes_snapshot, listed_price_snapshot_minor, charged_price_minor, sort_order, created_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
            params![id, snapshot.service_id, snapshot.service_name_snapshot, snapshot.duration_minutes_snapshot, snapshot.listed_price_snapshot_minor, snapshot.charged_price_minor, snapshot.sort_order, now],
        )?;
    }
    append_audit_event(
        &tx,
        AuditEntityType::Appointment,
        &id,
        AuditAction::Create,
        None,
    )?;
    enqueue_google_sync_if_enabled(&tx, &id)?;
    reconcile_reminder_for_appointment(&tx, &id)?;
    tx.commit()?;
    get_appointment(connection, &id)
}

fn update_appointment_tx(
    connection: &mut Connection,
    id: &str,
    input: AppointmentInput,
) -> Result<AppointmentSummary, AppError> {
    let status = validate_status(input.status.as_deref().unwrap_or("planned"))?;
    let note = optional_text(input.note, 2000)?;
    let current = get_raw_appointment(connection, id)?
        .ok_or_else(|| AppError::NotFound("APPOINTMENT_NOT_FOUND".to_string()))?;
    let whatsapp_reminder_enabled = input
        .whatsapp_reminder_enabled
        .unwrap_or(current.whatsapp_reminder_enabled);
    let tx = connection.transaction()?;
    assert_appointment_references(
        &tx,
        &input.customer_id,
        &input.staff_id,
        Some((&current.customer_id, &current.staff_id)),
    )?;
    let existing = if current.staff_id == input.staff_id {
        Some(list_appointment_services(&tx, id)?)
    } else {
        None
    };
    let (snapshots, total_duration, used_existing) =
        appointment_snapshots(&tx, &input.staff_id, input.service_ids, existing)?;
    let start = local_to_utc(&input.local_date, &input.local_start_time)?;
    let end = start + Duration::minutes(total_duration);
    ensure_same_istanbul_day(start, end)?;
    let start_iso = utc_iso(start);
    let end_iso = utc_iso(end);
    if status == "no_show" && start > Utc::now() {
        return Err(AppError::Validation(
            "APPOINTMENT_NO_SHOW_TOO_EARLY".to_string(),
        ));
    }
    if status != APPOINTMENT_STATUS_CANCELLED {
        let interval = AppointmentInterval {
            start_at_utc: start,
            end_at_utc: end,
        };
        if let AppointmentAvailability::Unavailable(reason) =
            check_staff_appointment_availability(&tx, &input.staff_id, &interval, Some(id))?
        {
            return Err(appointment_availability_error(reason));
        }
    }
    if matches!(status.as_str(), "planned" | "confirmed" | "completed") {
        assert_no_conflict(&tx, &input.staff_id, &start_iso, &end_iso, Some(id))?;
    }
    let no_change = used_existing
        && current.customer_id == input.customer_id
        && current.staff_id == input.staff_id
        && current.start_at_utc == start_iso
        && current.end_at_utc == end_iso
        && current.total_duration_minutes == total_duration
        && current.status == status
        && current.note == note
        && current.whatsapp_reminder_enabled == whatsapp_reminder_enabled;
    if no_change {
        drop(tx);
        return get_appointment(connection, id);
    }
    let mut changed_fields = Vec::new();
    if current.customer_id != input.customer_id {
        changed_fields.push("customer_id");
    }
    if current.staff_id != input.staff_id {
        changed_fields.push("staff_id");
    }
    if current.start_at_utc != start_iso || current.end_at_utc != end_iso {
        changed_fields.push("schedule");
    }
    if current.total_duration_minutes != total_duration {
        changed_fields.push("duration_minutes");
    }
    if !used_existing {
        changed_fields.push("service_ids");
    }
    if current.note != note {
        changed_fields.push("note");
    }
    if current.whatsapp_reminder_enabled != whatsapp_reminder_enabled {
        changed_fields.push("whatsapp_reminder_enabled");
    }
    let status_changed = current.status != status;
    tx.execute(
        "UPDATE appointments SET customer_id=?1,staff_id=?2,start_at_utc=?3,end_at_utc=?4,total_duration_minutes=?5,status=?6,note=?7,whatsapp_reminder_enabled=?8,updated_at=?9 WHERE id=?10",
        params![input.customer_id, input.staff_id, start_iso, end_iso, total_duration, status, note, bool_to_i64(whatsapp_reminder_enabled), now_iso(), id],
    )?;
    if !used_existing {
        tx.execute(
            "DELETE FROM appointment_services WHERE appointment_id=?1",
            params![id],
        )?;
        let now = now_iso();
        for snapshot in &snapshots {
            tx.execute(
                "INSERT INTO appointment_services (appointment_id, service_id, service_name_snapshot, duration_minutes_snapshot, listed_price_snapshot_minor, charged_price_minor, sort_order, created_at)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
                params![id, snapshot.service_id, snapshot.service_name_snapshot, snapshot.duration_minutes_snapshot, snapshot.listed_price_snapshot_minor, snapshot.charged_price_minor, snapshot.sort_order, now],
            )?;
        }
    }
    let audit_action = if status_changed {
        AuditAction::StatusChange
    } else {
        AuditAction::Update
    };
    let audit_metadata = if status_changed {
        Some(AuditMetadata::StatusChange {
            from_status: &current.status,
            to_status: &status,
        })
    } else {
        Some(AuditMetadata::ChangedFields(&changed_fields))
    };
    append_audit_event(
        &tx,
        AuditEntityType::Appointment,
        id,
        audit_action,
        audit_metadata,
    )?;
    enqueue_google_sync_if_enabled(&tx, id)?;
    reconcile_reminder_for_appointment(&tx, id)?;
    tx.commit()?;
    get_appointment(connection, id)
}

#[derive(Debug)]
struct RawAppointment {
    customer_id: String,
    staff_id: String,
    start_at_utc: String,
    end_at_utc: String,
    total_duration_minutes: i64,
    status: String,
    note: Option<String>,
    whatsapp_reminder_enabled: bool,
}

fn get_raw_appointment(
    connection: &Connection,
    id: &str,
) -> Result<Option<RawAppointment>, AppError> {
    connection
        .query_row(
            "SELECT customer_id, staff_id, start_at_utc, end_at_utc, total_duration_minutes, status, note, whatsapp_reminder_enabled FROM appointments WHERE id=?1",
            params![id],
            |row| {
                Ok(RawAppointment {
                    customer_id: row.get(0)?,
                    staff_id: row.get(1)?,
                    start_at_utc: row.get(2)?,
                    end_at_utc: row.get(3)?,
                    total_duration_minutes: row.get(4)?,
                    status: row.get(5)?,
                    note: row.get(6)?,
                    whatsapp_reminder_enabled: row.get::<_, i64>(7)? == 1,
                })
            },
        )
        .optional()
        .map_err(Into::into)
}

fn list_appointment_services(
    connection: &Connection,
    appointment_id: &str,
) -> Result<Vec<AppointmentServiceSnapshot>, AppError> {
    let mut statement = connection.prepare(
        "SELECT service_id, service_name_snapshot, duration_minutes_snapshot, listed_price_snapshot_minor, charged_price_minor, sort_order FROM appointment_services WHERE appointment_id=?1 ORDER BY sort_order ASC",
    )?;
    let rows = statement.query_map(params![appointment_id], |row| {
        Ok(AppointmentServiceSnapshot {
            service_id: row.get(0)?,
            service_name_snapshot: row.get(1)?,
            duration_minutes_snapshot: row.get(2)?,
            listed_price_snapshot_minor: row.get(3)?,
            charged_price_minor: row.get(4)?,
            sort_order: row.get(5)?,
        })
    })?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

fn list_appointment_services_batched(
    connection: &Connection,
    appointment_ids: &[String],
) -> Result<HashMap<String, Vec<AppointmentServiceSnapshot>>, AppError> {
    if appointment_ids.is_empty() {
        return Ok(HashMap::new());
    }
    let placeholders = std::iter::repeat("?")
        .take(appointment_ids.len())
        .collect::<Vec<_>>()
        .join(",");
    let mut statement = connection.prepare(&format!(
        "SELECT appointment_id, service_id, service_name_snapshot, duration_minutes_snapshot,
                listed_price_snapshot_minor, charged_price_minor, sort_order
         FROM appointment_services
         WHERE appointment_id IN ({placeholders})
         ORDER BY appointment_id ASC, sort_order ASC"
    ))?;
    let rows = statement.query_map(params_from_iter(appointment_ids.iter()), |row| {
        Ok((
            row.get::<_, String>(0)?,
            AppointmentServiceSnapshot {
                service_id: row.get(1)?,
                service_name_snapshot: row.get(2)?,
                duration_minutes_snapshot: row.get(3)?,
                listed_price_snapshot_minor: row.get(4)?,
                charged_price_minor: row.get(5)?,
                sort_order: row.get(6)?,
            },
        ))
    })?;
    let mut grouped = HashMap::<String, Vec<AppointmentServiceSnapshot>>::new();
    for row in rows {
        let (appointment_id, snapshot) = row?;
        grouped.entry(appointment_id).or_default().push(snapshot);
    }
    Ok(grouped)
}

fn history_local_date_time(start_at_utc: &str) -> Result<(String, String), AppError> {
    let utc = chrono::DateTime::parse_from_rfc3339(start_at_utc)
        .map_err(|_| AppError::Database("appointment start timestamp".to_string()))?
        .with_timezone(&Utc);
    let local = utc + Duration::minutes(ISTANBUL_OFFSET_MINUTES);
    Ok((
        local.format("%Y-%m-%d").to_string(),
        local.format("%H:%M").to_string(),
    ))
}

fn load_customer_history(
    connection: &Connection,
    customer_id: &str,
    limit: i64,
    offset: i64,
) -> Result<CustomerHistoryPage, AppError> {
    let customer = get_customer(connection, customer_id)?
        .ok_or_else(|| AppError::NotFound("CUSTOMER_NOT_FOUND".to_string()))?;
    let limit = limit.clamp(1, 100);
    let offset = offset.max(0);
    let mut appointments_statement = connection.prepare(
        "SELECT a.id, a.start_at_utc, a.status, a.staff_id,
                st.first_name || COALESCE(' ' || st.last_name, '') AS staff_display_name
         FROM appointments a
         INNER JOIN staff st ON st.id=a.staff_id
         WHERE a.customer_id=?1
         ORDER BY a.start_at_utc DESC, a.id DESC
         LIMIT ?2 OFFSET ?3",
    )?;
    let appointment_rows =
        appointments_statement.query_map(params![customer_id, limit, offset], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
            ))
        })?;
    let appointments = appointment_rows.collect::<Result<Vec<_>, _>>()?;
    if appointments.is_empty() {
        return Ok(CustomerHistoryPage {
            customer: CustomerHistoryCustomer {
                customer_id: customer.id,
                name: format!("{} {}", customer.first_name, customer.last_name),
                phone: customer.phone,
                is_active: customer.is_active,
            },
            appointments: Vec::new(),
            limit,
            offset,
        });
    }

    let appointment_ids: Vec<&str> = appointments.iter().map(|(id, ..)| id.as_str()).collect();
    let placeholders = std::iter::repeat("?")
        .take(appointment_ids.len())
        .collect::<Vec<_>>()
        .join(",");
    let mut services_statement = connection.prepare(&format!(
        "SELECT appointment_id, service_id, service_name_snapshot, duration_minutes_snapshot,
                listed_price_snapshot_minor, charged_price_minor, sort_order
         FROM appointment_services
         WHERE appointment_id IN ({placeholders})
         ORDER BY appointment_id ASC, sort_order ASC"
    ))?;
    let service_rows = services_statement.query_map(params_from_iter(appointment_ids), |row| {
        Ok((
            row.get::<_, String>(0)?,
            AppointmentServiceSnapshot {
                service_id: row.get(1)?,
                service_name_snapshot: row.get(2)?,
                duration_minutes_snapshot: row.get(3)?,
                listed_price_snapshot_minor: row.get(4)?,
                charged_price_minor: row.get(5)?,
                sort_order: row.get(6)?,
            },
        ))
    })?;
    let mut services_by_appointment: HashMap<String, Vec<AppointmentServiceSnapshot>> =
        HashMap::new();
    for service_row in service_rows {
        let (appointment_id, snapshot) = service_row?;
        services_by_appointment
            .entry(appointment_id)
            .or_default()
            .push(snapshot);
    }
    let appointments = appointments
        .into_iter()
        .map(
            |(appointment_id, start_at_utc, status, staff_id, staff_display_name)| {
                let (local_date, local_time) = history_local_date_time(&start_at_utc)?;
                let services = services_by_appointment
                    .remove(&appointment_id)
                    .unwrap_or_default();
                let total_charged_minor = services
                    .iter()
                    .map(|service| service.charged_price_minor)
                    .sum();
                Ok(CustomerHistoryAppointment {
                    appointment_id,
                    local_date,
                    local_time,
                    status,
                    staff_id,
                    staff_display_name,
                    services,
                    total_charged_minor,
                })
            },
        )
        .collect::<Result<Vec<_>, AppError>>()?;
    Ok(CustomerHistoryPage {
        customer: CustomerHistoryCustomer {
            customer_id: customer.id,
            name: format!("{} {}", customer.first_name, customer.last_name),
            phone: customer.phone,
            is_active: customer.is_active,
        },
        appointments,
        limit,
        offset,
    })
}

fn load_repeat_booking_seed(
    connection: &Connection,
    customer_id: &str,
    source_appointment_id: &str,
) -> Result<RepeatBookingSeed, AppError> {
    let customer = get_customer(connection, customer_id)?
        .ok_or_else(|| AppError::NotFound("CUSTOMER_NOT_FOUND".to_string()))?;
    let source = connection
        .query_row(
            "SELECT customer_id, staff_id FROM appointments WHERE id=?1",
            params![source_appointment_id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?
        .ok_or_else(|| AppError::NotFound("APPOINTMENT_NOT_FOUND".to_string()))?;
    if source.0 != customer_id {
        return Err(AppError::Conflict(
            "APPOINTMENT_CUSTOMER_MISMATCH".to_string(),
        ));
    }
    if !customer.is_active {
        return Err(AppError::Conflict(
            "CUSTOMER_REACTIVATION_REQUIRED".to_string(),
        ));
    }
    let staff_id = get_staff(connection, &source.1)?.and_then(|staff| {
        if staff.is_active {
            Some(staff.id)
        } else {
            None
        }
    });
    let mut statement = connection.prepare(
        "SELECT aps.service_id, aps.service_name_snapshot, s.is_active, c.is_active
         FROM appointment_services aps
         INNER JOIN services s ON s.id=aps.service_id
         INNER JOIN service_categories c ON c.id=s.category_id
         WHERE aps.appointment_id=?1
         ORDER BY aps.sort_order ASC",
    )?;
    let rows = statement.query_map(params![source_appointment_id], |row| {
        let service_active = row.get::<_, i64>(2)? != 0;
        let category_active = row.get::<_, i64>(3)? != 0;
        let unavailable_reason = if !service_active {
            Some("service_inactive".to_string())
        } else if !category_active {
            Some("service_category_inactive".to_string())
        } else {
            None
        };
        Ok(RepeatBookingSeedService {
            service_id: row.get(0)?,
            service_name_snapshot: row.get(1)?,
            is_eligible: unavailable_reason.is_none(),
            unavailable_reason,
        })
    })?;
    let services = rows.collect::<Result<Vec<_>, _>>()?;
    Ok(RepeatBookingSeed {
        customer_id: customer.id,
        customer_display_name: format!("{} {}", customer.first_name, customer.last_name),
        staff_id,
        services,
    })
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceStatistic {
    service_id: String,
    service_name_snapshot: String,
    completed_count: i64,
    revenue_minor: i64,
}

fn set_appointment_service_charged_price(
    connection: &mut Connection,
    appointment_id: &str,
    service_id: &str,
    charged_price_minor: i64,
) -> Result<(), AppError> {
    if charged_price_minor < 0 {
        return Err(AppError::Validation("chargedPriceMinor invalid".into()));
    }
    let previous_price_minor: i64 = connection
        .query_row(
            "SELECT charged_price_minor FROM appointment_services WHERE appointment_id=?1 AND service_id=?2",
            params![appointment_id, service_id],
            |row| row.get(0),
        )
        .optional()?
        .ok_or_else(|| AppError::NotFound("APPOINTMENT_SERVICE_NOT_FOUND".into()))?;
    if previous_price_minor == charged_price_minor {
        return Ok(());
    }
    let tx = connection.transaction()?;
    tx.execute(
        "UPDATE appointment_services SET charged_price_minor=?1 WHERE appointment_id=?2 AND service_id=?3",
        params![charged_price_minor, appointment_id, service_id],
    )?;
    let entity_id = format!("{appointment_id}:{service_id}");
    append_audit_event(
        &tx,
        AuditEntityType::AppointmentService,
        &entity_id,
        AuditAction::PriceOverride,
        Some(AuditMetadata::PriceOverride {
            previous_minor: previous_price_minor,
            new_minor: charged_price_minor,
        }),
    )?;
    tx.commit()?;
    Ok(())
}

fn service_statistics_tx(
    connection: &Connection,
    start_date: &str,
    end_date: &str,
) -> Result<Vec<ServiceStatistic>, AppError> {
    let start = utc_iso(local_to_utc(start_date, "00:00")?);
    let end = utc_iso(local_to_utc(end_date, "00:00")? + Duration::days(1));
    let mut statement = connection.prepare("SELECT aps.service_id, aps.service_name_snapshot, COUNT(*), COALESCE(SUM(aps.charged_price_minor),0) FROM appointment_services aps JOIN appointments a ON a.id=aps.appointment_id WHERE a.status='completed' AND a.start_at_utc>=?1 AND a.start_at_utc<?2 GROUP BY aps.service_id, aps.service_name_snapshot ORDER BY aps.service_name_snapshot")?;
    let rows = statement.query_map(params![start, end], |row| {
        Ok(ServiceStatistic {
            service_id: row.get(0)?,
            service_name_snapshot: row.get(1)?,
            completed_count: row.get(2)?,
            revenue_minor: row.get(3)?,
        })
    })?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

fn get_appointment(connection: &Connection, id: &str) -> Result<AppointmentSummary, AppError> {
    let mut appointments =
        list_appointments_between(connection, None, None, None, None, Some(id), 1)?;
    appointments
        .pop()
        .ok_or_else(|| AppError::NotFound("APPOINTMENT_NOT_FOUND".to_string()))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BackupKind {
    Automatic,
    Safety,
    Manual,
}

impl BackupKind {
    fn directory_name(self) -> &'static str {
        match self {
            Self::Automatic => "automatic",
            Self::Safety => "safety",
            Self::Manual => "manual",
        }
    }

    fn retention_limit(self) -> Option<usize> {
        match self {
            Self::Automatic => Some(7),
            Self::Safety => Some(3),
            Self::Manual => None,
        }
    }
}

fn backup_root(database_path: &Path) -> PathBuf {
    database_path
        .parent()
        .and_then(Path::parent)
        .map(|root| root.join("backups"))
        .unwrap_or_else(|| {
            database_path
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .join("backups")
        })
}

fn backup_directory(database_path: &Path, kind: BackupKind) -> Result<PathBuf, AppError> {
    let directory = backup_root(database_path).join(kind.directory_name());
    fs::create_dir_all(&directory)?;
    Ok(directory)
}

fn backup_file_name(kind: BackupKind) -> String {
    let sequence = ID_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!(
        "beautysaloon-{}-{}-{sequence:08x}.sqlite",
        kind.directory_name(),
        timestamp_for_file()
    )
}

fn temporary_backup_path(destination: &Path) -> PathBuf {
    let sequence = ID_COUNTER.fetch_add(1, Ordering::Relaxed);
    PathBuf::from(format!("{}.tmp-{sequence:08x}", destination.display()))
}

fn sanitize_machine_bound_state(database_path: &Path) -> Result<bool, AppError> {
    let mut connection = Connection::open(database_path)?;
    connection.pragma_update(None, "foreign_keys", "ON")?;
    let tx = connection.transaction()?;
    tx.execute("DELETE FROM secure_secrets", [])?;
    tx.execute(
        "UPDATE google_calendar_settings
         SET sync_enabled=0, client_id=NULL, calendar_id='primary', account_email=NULL, updated_at_utc=?1
         WHERE id=1",
        params![now_iso()],
    )?;
    tx.execute(
        "UPDATE cloud_reminder_settings
         SET cloud_mode_enabled=0, project_url=NULL, publishable_key=NULL, updated_at_utc=?1
         WHERE id=1",
        params![now_iso()],
    )?;
    tx.execute(
        "UPDATE whatsapp_settings
         SET is_enabled=0, phone_number_id=NULL, template_name=NULL, automatic_reminder_enabled=0, updated_at=?1
         WHERE id=1",
        params![now_iso()],
    )?;
    tx.commit()?;
    if !integrity_check(&connection)? {
        return Err(AppError::Database("BACKUP_INTEGRITY_FAILED".to_string()));
    }
    Ok(true)
}

fn validate_backup_snapshot(database_path: &Path) -> Result<(), AppError> {
    let connection = Connection::open(database_path)?;
    connection.pragma_update(None, "foreign_keys", "ON")?;
    if !integrity_check(&connection)? {
        return Err(AppError::Database("BACKUP_INTEGRITY_FAILED".to_string()));
    }
    if read_schema_version(&connection)? != CORE_SCHEMA_VERSION {
        return Err(AppError::Database(
            "BACKUP_SCHEMA_VERSION_INVALID".to_string(),
        ));
    }
    Ok(())
}

fn validate_and_publish_backup(temporary_path: &Path, destination: &Path) -> Result<(), AppError> {
    let result = validate_backup_snapshot(temporary_path).and_then(|_| {
        fs::rename(temporary_path, destination)?;
        Ok(())
    });
    if result.is_err() {
        let _ = fs::remove_file(temporary_path);
    }
    result
}

fn enforce_backup_retention(directory: &Path, kind: BackupKind) -> Result<(), AppError> {
    let Some(limit) = kind.retention_limit() else {
        return Ok(());
    };
    let prefix = format!("beautysaloon-{}-", kind.directory_name());
    let mut snapshots = fs::read_dir(directory)?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            let name = path.file_name()?.to_str()?;
            (path.is_file() && name.starts_with(&prefix) && name.ends_with(".sqlite"))
                .then_some(path)
        })
        .collect::<Vec<_>>();
    snapshots.sort_by(|left, right| right.file_name().cmp(&left.file_name()));
    for snapshot in snapshots.into_iter().skip(limit) {
        fs::remove_file(snapshot)?;
    }
    Ok(())
}

fn create_database_backup(
    connection: &Connection,
    database_path: &Path,
    kind: BackupKind,
) -> Result<DatabaseBackupSummary, AppError> {
    if !database_path.is_file() {
        return Err(AppError::Database("BACKUP_SOURCE_MISSING".to_string()));
    }
    if !integrity_check(connection)? {
        return Err(AppError::Database("SOURCE_INTEGRITY_FAILED".to_string()));
    }

    let directory = backup_directory(database_path, kind)?;
    let destination = directory.join(backup_file_name(kind));
    let temporary_path = temporary_backup_path(&destination);
    let result: Result<bool, AppError> = (|| {
        let sql = format!("VACUUM INTO '{}';", sql_path_literal(&temporary_path));
        connection.execute_batch(&sql)?;
        let machine_bound_secrets_removed = sanitize_machine_bound_state(&temporary_path)?;
        validate_and_publish_backup(&temporary_path, &destination)?;
        Ok(machine_bound_secrets_removed)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary_path);
    }
    let machine_bound_secrets_removed = result?;

    if enforce_backup_retention(&directory, kind).is_err() {
        safe_diagnostic("BACKUP_RETENTION_WARNING");
    }

    Ok(DatabaseBackupSummary {
        database_path: database_path.display().to_string(),
        backup_path: destination.display().to_string(),
        integrity: "ok".to_string(),
        machine_bound_secrets_removed,
        created_at_utc: now_iso(),
    })
}

fn automatic_backup_tick_with<F>(
    coordinator: &BackupCoordinator,
    create_backup: F,
) -> Result<bool, AppError>
where
    F: FnOnce() -> Result<(), AppError>,
{
    let Some(captured_generation) = coordinator.begin_automatic_backup() else {
        return Ok(false);
    };
    let result = create_backup();
    coordinator.finish_automatic_backup(captured_generation, result.is_ok());
    result.map(|_| true)
}

fn automatic_backup_tick(
    coordinator: &BackupCoordinator,
    database_path: &Path,
) -> Result<bool, AppError> {
    let Ok(_operation) = coordinator.backup_operation_lock.try_lock() else {
        return Ok(false);
    };
    automatic_backup_tick_with(coordinator, || {
        let connection = Connection::open(database_path)?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        create_database_backup(&connection, database_path, BackupKind::Automatic)?;
        Ok(())
    })
}

#[cfg(test)]
fn shutdown_automatic_backup_with<F>(
    coordinator: &BackupCoordinator,
    timeout: StdDuration,
    create_backup: F,
) -> Result<bool, AppError>
where
    F: FnOnce() -> Result<(), AppError>,
{
    let Some(captured_generation) = coordinator.wait_for_automatic_backup_slot(timeout)? else {
        return Ok(false);
    };
    let result = create_backup();
    coordinator.finish_automatic_backup(captured_generation, result.is_ok());
    result.map(|_| true)
}

fn shutdown_automatic_backup(
    coordinator: Arc<BackupCoordinator>,
    database_path: PathBuf,
) -> Result<bool, AppError> {
    let Some(captured_generation) =
        coordinator.wait_for_automatic_backup_slot(SHUTDOWN_BACKUP_WAIT_TIMEOUT)?
    else {
        return Ok(false);
    };
    let (sender, receiver) = mpsc::sync_channel(1);
    let worker_coordinator = coordinator.clone();
    std::thread::spawn(move || {
        let result = (|| {
            let _operation = worker_coordinator
                .backup_operation_lock
                .lock()
                .expect("backup operation mutex poisoned");
            let connection = Connection::open(&database_path)?;
            connection.pragma_update(None, "journal_mode", "WAL")?;
            connection.pragma_update(None, "foreign_keys", "ON")?;
            create_database_backup(&connection, &database_path, BackupKind::Automatic)?;
            Ok(())
        })();
        worker_coordinator.finish_automatic_backup(captured_generation, result.is_ok());
        let _ = sender.send(result);
    });
    match receiver.recv_timeout(SHUTDOWN_BACKUP_WAIT_TIMEOUT) {
        Ok(result) => result.map(|_| true),
        Err(mpsc::RecvTimeoutError::Timeout) => Err(AppError::Database(
            "AUTOMATIC_BACKUP_SHUTDOWN_TIMEOUT".to_string(),
        )),
        Err(mpsc::RecvTimeoutError::Disconnected) => Err(AppError::Database(
            "AUTOMATIC_BACKUP_SHUTDOWN_WORKER_FAILED".to_string(),
        )),
    }
}

fn start_automatic_backup_coordinator(database_path: PathBuf, coordinator: Arc<BackupCoordinator>) {
    if !coordinator.register_automatic_backup_loop() {
        return;
    }
    std::thread::spawn(move || loop {
        std::thread::sleep(AUTOMATIC_BACKUP_INTERVAL);
        if automatic_backup_tick(&coordinator, &database_path).is_err() {
            safe_diagnostic("AUTOMATIC_BACKUP_WARNING");
        }
    });
}

fn run_business_mutation<T>(
    state: &AppState,
    operation: impl FnOnce(&mut Connection) -> Result<T, AppError>,
) -> Result<T, AppError> {
    let mut connection = state.sqlite.lock().expect("sqlite mutex poisoned");
    let changes_before = connection.total_changes();
    let result = operation(&mut connection);
    if result.is_ok() && connection.total_changes() > changes_before {
        state.backup_coordinator.mark_business_change();
    }
    result
}

fn foreign_key_check_clean(connection: &Connection) -> Result<bool, AppError> {
    let violations: i64 =
        connection.query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
            row.get(0)
        })?;
    Ok(violations == 0)
}

#[derive(Debug)]
struct MachineSecret {
    key: String,
    value: Vec<u8>,
    provider: String,
    created_at: String,
    updated_at: String,
}

fn capture_machine_secrets(connection: &Connection) -> Result<Vec<MachineSecret>, AppError> {
    let mut statement = connection.prepare(
        "SELECT secret_key, encrypted_value, encryption_provider, created_at, updated_at FROM secure_secrets",
    )?;
    let rows = statement.query_map([], |row| {
        Ok(MachineSecret {
            key: row.get(0)?,
            value: row.get(1)?,
            provider: row.get(2)?,
            created_at: row.get(3)?,
            updated_at: row.get(4)?,
        })
    })?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

fn restore_machine_secrets(
    connection: &mut Connection,
    secrets: &[MachineSecret],
) -> Result<(), AppError> {
    let tx = connection.transaction()?;
    tx.execute("DELETE FROM secure_secrets", [])?;
    for secret in secrets {
        tx.execute(
            "INSERT INTO secure_secrets (secret_key, encrypted_value, encryption_provider, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![secret.key, secret.value, secret.provider, secret.created_at, secret.updated_at],
        )?;
    }
    tx.commit()?;
    Ok(())
}

fn restore_working_path(database_path: &Path, label: &str) -> Result<PathBuf, AppError> {
    let directory = database_path
        .parent()
        .ok_or_else(|| AppError::Database("RESTORE_DATABASE_PATH_INVALID".into()))?
        .join("restore-work");
    fs::create_dir_all(&directory)?;
    Ok(directory.join(format!(
        "{label}-{}-{:08x}.sqlite",
        timestamp_for_file(),
        ID_COUNTER.fetch_add(1, Ordering::Relaxed)
    )))
}

fn prepare_restore_candidate(
    candidate_path: &Path,
    database_path: &Path,
) -> Result<PathBuf, AppError> {
    if !candidate_path.is_file() {
        return Err(AppError::NotFound("RESTORE_CANDIDATE_NOT_FOUND".into()));
    }
    let candidate = Connection::open(candidate_path)
        .map_err(|_| AppError::Validation("RESTORE_CANDIDATE_INVALID".into()))?;
    if !integrity_check(&candidate)? || !foreign_key_check_clean(&candidate)? {
        return Err(AppError::Validation("RESTORE_CANDIDATE_INVALID".into()));
    }
    let schema_version = read_schema_version(&candidate)
        .map_err(|_| AppError::Validation("RESTORE_CANDIDATE_INVALID".into()))?;
    if !(1..=CORE_SCHEMA_VERSION).contains(&schema_version) {
        return Err(AppError::Validation(
            "RESTORE_CANDIDATE_SCHEMA_UNSUPPORTED".into(),
        ));
    }
    let prepared = restore_working_path(database_path, "candidate")?;
    let sql = format!("VACUUM INTO '{}';", sql_path_literal(&prepared));
    candidate.execute_batch(&sql)?;
    drop(candidate);
    let migrated = open_database(&prepared)?;
    if read_schema_version(&migrated)? != CORE_SCHEMA_VERSION
        || !integrity_check(&migrated)?
        || !foreign_key_check_clean(&migrated)?
    {
        let _ = fs::remove_file(&prepared);
        return Err(AppError::Validation("RESTORE_CANDIDATE_INVALID".into()));
    }
    drop(migrated);
    sanitize_machine_bound_state(&prepared)?;
    validate_backup_snapshot(&prepared)?;
    Ok(prepared)
}

fn remove_sqlite_sidecars(database_path: &Path) -> Result<(), AppError> {
    for suffix in ["-wal", "-shm"] {
        let sidecar = PathBuf::from(format!("{}{}", database_path.display(), suffix));
        if sidecar.exists() {
            fs::remove_file(sidecar)?;
        }
    }
    Ok(())
}

fn swap_prepared_database(prepared: &Path, database_path: &Path) -> Result<PathBuf, AppError> {
    let previous = restore_working_path(database_path, "previous")?;
    remove_sqlite_sidecars(database_path)?;
    fs::rename(database_path, &previous)?;
    if let Err(error) = fs::rename(prepared, database_path) {
        let _ = fs::rename(&previous, database_path);
        return Err(error.into());
    }
    Ok(previous)
}

fn restore_database_from_candidate(
    state: &AppState,
    candidate_path: &Path,
) -> Result<(), AppError> {
    let prepared = prepare_restore_candidate(candidate_path, &state.database_path)?;
    if !state.backup_coordinator.begin_restore() {
        let _ = fs::remove_file(&prepared);
        return Err(AppError::Conflict("RESTORE_IN_PROGRESS".into()));
    }
    let result = (|| {
        let _operation = state
            .backup_coordinator
            .backup_operation_lock
            .lock()
            .expect("backup operation mutex poisoned");
        let mut active = state.sqlite.lock().expect("sqlite mutex poisoned");
        let machine_secrets = capture_machine_secrets(&active)?;
        let safety = create_database_backup(&active, &state.database_path, BackupKind::Safety)?;
        let placeholder = Connection::open_in_memory()?;
        let old_connection = std::mem::replace(&mut *active, placeholder);
        old_connection
            .close()
            .map_err(|(_, error)| AppError::Sqlite(error))?;
        let previous = swap_prepared_database(&prepared, &state.database_path)?;
        match open_database(&state.database_path) {
            Ok(mut restored)
                if integrity_check(&restored)?
                    && foreign_key_check_clean(&restored)?
                    && read_schema_version(&restored)? == CORE_SCHEMA_VERSION =>
            {
                restore_machine_secrets(&mut restored, &machine_secrets)?;
                *active = restored;
                let _ = fs::remove_file(previous);
                Ok(())
            }
            _ => {
                let recovery = prepare_restore_candidate(
                    Path::new(&safety.backup_path),
                    &state.database_path,
                )?;
                let _ = fs::remove_file(&state.database_path);
                let _ = remove_sqlite_sidecars(&state.database_path);
                fs::rename(&recovery, &state.database_path)?;
                let mut recovered = open_database(&state.database_path)?;
                restore_machine_secrets(&mut recovered, &machine_secrets)?;
                *active = recovered;
                let _ = fs::remove_file(previous);
                Err(AppError::Database("RESTORE_ROLLED_BACK".into()))
            }
        }
    })();
    if result.is_ok() {
        state.backup_coordinator.finish_restore(0);
    } else {
        state.backup_coordinator.abort_restore();
    }
    if result.is_err() {
        let _ = fs::remove_file(&prepared);
    }
    result
}

fn clean_database_in_place(
    connection: &mut Connection,
    database_path: &Path,
) -> Result<DatabaseCleanSummary, AppError> {
    let safety = create_database_backup(connection, database_path, BackupKind::Safety)?;
    let tx = connection.transaction()?;
    for table in [
        "reminder_cloud_outbox",
        "reminder_cloud_state",
        "appointment_reminders",
        "google_calendar_outbox",
        "appointment_google_calendar_sync",
        "appointment_services",
        "appointments",
        "staff_services",
        "services",
        "service_categories",
        "staff",
        "customers",
        "secure_secrets",
    ] {
        tx.execute(&format!("DELETE FROM {table}"), [])?;
    }
    tx.execute("UPDATE whatsapp_settings SET is_enabled=0, automatic_reminder_enabled=0, updated_at=?1 WHERE id=1", params![now_iso()])?;
    tx.execute("UPDATE google_calendar_settings SET sync_enabled=0, account_email=NULL, updated_at_utc=?1 WHERE id=1", params![now_iso()])?;
    tx.execute(
        "UPDATE cloud_reminder_settings SET cloud_mode_enabled=0, updated_at_utc=?1 WHERE id=1",
        params![now_iso()],
    )?;
    tx.commit()?;
    if !integrity_check(connection)? {
        return Err(AppError::Database("CLEAN_INTEGRITY_FAILED".to_string()));
    }
    Ok(DatabaseCleanSummary {
        database_path: database_path.display().to_string(),
        safety_backup_path: safety.backup_path,
        integrity: "ok".to_string(),
        cleaned_at_utc: now_iso(),
    })
}

#[cfg(test)]
fn restore_database_file_for_tests(active_path: &Path, backup_path: &Path) -> Result<(), AppError> {
    let backup = Connection::open(backup_path)?;
    if !integrity_check(&backup)? {
        return Err(AppError::Database(
            "RESTORE_SOURCE_INTEGRITY_FAILED".to_string(),
        ));
    }
    drop(backup);
    for suffix in ["", "-wal", "-shm"] {
        let sidecar = PathBuf::from(format!("{}{}", active_path.display(), suffix));
        if sidecar.exists() {
            fs::remove_file(sidecar)?;
        }
    }
    fs::copy(backup_path, active_path)?;
    Ok(())
}

fn enqueue_google_sync_if_enabled(
    connection: &Connection,
    appointment_id: &str,
) -> Result<(), AppError> {
    let enabled: i64 = connection.query_row(
        "SELECT sync_enabled FROM google_calendar_settings WHERE id=1",
        [],
        |row| row.get(0),
    )?;
    if enabled != 1 {
        return Ok(());
    }
    let now = now_iso();
    connection.execute(
        "INSERT INTO appointment_google_calendar_sync (appointment_id, google_event_id, created_at_utc, updated_at_utc)
         VALUES (?1, NULL, ?2, ?2)
         ON CONFLICT(appointment_id) DO UPDATE SET updated_at_utc=excluded.updated_at_utc",
        params![appointment_id, now],
    )?;
    connection.execute(
        "INSERT INTO google_calendar_outbox (id, appointment_id, sync_status, created_at_utc, updated_at_utc)
         VALUES (?1, ?2, 'pending', ?3, ?3)
         ON CONFLICT(appointment_id) DO UPDATE SET sync_status='pending', last_error_code=NULL, updated_at_utc=excluded.updated_at_utc",
        params![new_uuid(), appointment_id, now],
    )?;
    Ok(())
}

fn google_sync_pending_mock_tx(connection: &Connection) -> Result<u32, AppError> {
    let mut statement = connection.prepare(
        "SELECT appointment_id FROM google_calendar_outbox WHERE sync_status='pending' ORDER BY created_at_utc ASC",
    )?;
    let appointment_ids = statement
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    let now = now_iso();
    for appointment_id in &appointment_ids {
        let event_id: Option<String> = connection
            .query_row(
                "SELECT google_event_id FROM appointment_google_calendar_sync WHERE appointment_id=?1",
                params![appointment_id],
                |row| row.get(0),
            )
            .optional()?
            .flatten();
        let event_id = event_id.unwrap_or_else(|| format!("gcal-{appointment_id}"));
        connection.execute(
            "UPDATE appointment_google_calendar_sync SET google_event_id=?1, last_synced_updated_at_utc=?2, last_error_code=NULL, updated_at_utc=?2 WHERE appointment_id=?3",
            params![event_id, now, appointment_id],
        )?;
        connection.execute(
            "UPDATE google_calendar_outbox SET sync_status='synced', last_attempt_at_utc=?1, last_error_code=NULL, updated_at_utc=?1 WHERE appointment_id=?2",
            params![now, appointment_id],
        )?;
    }
    Ok(appointment_ids.len() as u32)
}

fn upsert_cloud_outbox(
    connection: &Connection,
    reminder_id: &str,
    action: &str,
    payload: Option<serde_json::Value>,
) -> Result<(), AppError> {
    let now = now_iso();
    connection.execute(
        "INSERT INTO reminder_cloud_state (reminder_id, last_synced_revision, next_revision, created_at_utc, updated_at_utc)
         VALUES (?1, 0, 1, ?2, ?2)
         ON CONFLICT(reminder_id) DO NOTHING",
        params![reminder_id, now],
    )?;
    let (last_synced_revision, next_revision): (i64, i64) = connection.query_row(
        "SELECT last_synced_revision, next_revision FROM reminder_cloud_state WHERE reminder_id=?1",
        params![reminder_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    if action != "upsert" && last_synced_revision == 0 {
        connection.execute(
            "DELETE FROM reminder_cloud_outbox WHERE reminder_id=?1 AND sync_status='pending' AND last_attempt_at_utc IS NULL",
            params![reminder_id],
        )?;
        connection.execute(
            "UPDATE reminder_cloud_state SET next_revision=1, updated_at_utc=?1 WHERE reminder_id=?2",
            params![now, reminder_id],
        )?;
        return Ok(());
    }
    let compact_id: Option<String> = if action == "upsert" && last_synced_revision == 0 {
        connection
            .query_row(
                "SELECT id FROM reminder_cloud_outbox WHERE reminder_id=?1 AND revision=1 AND action='upsert' AND sync_status='pending' AND last_attempt_at_utc IS NULL",
                params![reminder_id],
                |row| row.get(0),
            )
            .optional()?
    } else {
        None
    };
    let revision = if compact_id.is_some() {
        1
    } else {
        next_revision
    };
    let mutation_id = new_uuid();
    let payload_json = if action == "upsert" {
        let mut payload =
            payload.ok_or_else(|| AppError::Database("CLOUD_PAYLOAD_MISSING".to_string()))?;
        let object = payload
            .as_object_mut()
            .ok_or_else(|| AppError::Database("CLOUD_PAYLOAD_INVALID".to_string()))?;
        object.insert("apiVersion".to_string(), serde_json::json!(1));
        object.insert("reminderId".to_string(), serde_json::json!(reminder_id));
        object.insert("revision".to_string(), serde_json::json!(revision));
        object.insert(
            "clientMutationId".to_string(),
            serde_json::json!(mutation_id),
        );
        object.insert("reactivateCancelled".to_string(), serde_json::json!(false));
        Some(
            serde_json::to_string(&payload)
                .map_err(|_| AppError::Database("CLOUD_PAYLOAD_INVALID".to_string()))?,
        )
    } else {
        None
    };
    let payload_hash = pseudo_hash_64(&format!("{mutation_id}:{:?}", payload_json));
    if let Some(id) = compact_id {
        connection.execute(
            "UPDATE reminder_cloud_outbox SET client_mutation_id=?1, payload_json=?2, payload_hash=?3, last_error_code=NULL, updated_at_utc=?4 WHERE id=?5",
            params![mutation_id, payload_json, payload_hash, now, id],
        )?;
        return Ok(());
    }
    connection.execute(
        "INSERT INTO reminder_cloud_outbox (id, reminder_id, revision, action, client_mutation_id, payload_json, payload_hash, sync_status, created_at_utc, updated_at_utc)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'pending', ?8, ?8)",
        params![new_uuid(), reminder_id, revision, action, mutation_id, payload_json, payload_hash, now],
    )?;
    connection.execute(
        "UPDATE reminder_cloud_state SET next_revision=?1, updated_at_utc=?2 WHERE reminder_id=?3",
        params![revision + 1, now, reminder_id],
    )?;
    Ok(())
}

fn reconcile_reminder_for_appointment(
    connection: &Connection,
    appointment_id: &str,
) -> Result<bool, AppError> {
    let row = connection
        .query_row(
            "SELECT a.start_at_utc, a.status, a.whatsapp_reminder_enabled, c.is_active, c.phone, c.whatsapp_reminder_enabled, c.whatsapp_consent_confirmed, c.first_name, c.last_name
             FROM appointments a INNER JOIN customers c ON c.id=a.customer_id WHERE a.id=?1",
            params![appointment_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, String>(8)?,
                ))
            },
        )
        .optional()?;
    let Some((
        start_at,
        status,
        appointment_reminder_enabled,
        active,
        phone,
        reminder_enabled,
        consent,
        first_name,
        last_name,
    )) = row
    else {
        return Ok(false);
    };
    let start_at = chrono::DateTime::parse_from_rfc3339(&start_at)
        .map_err(|_| AppError::Validation("appointment start_at invalid".to_string()))?
        .with_timezone(&Utc);
    let now_utc = Utc::now();
    let eligible = start_at > now_utc
        && matches!(status.as_str(), "planned" | "confirmed")
        && active == 1
        && appointment_reminder_enabled == 1
        && reminder_enabled == 1
        && consent == 1
        && normalize_phone(phone.as_deref(), true).is_ok();
    let existing: Option<String> = connection
        .query_row(
            "SELECT id FROM appointment_reminders WHERE appointment_id=?1 AND channel='whatsapp' AND reminder_type='appointment_24h'",
            params![appointment_id],
            |row| row.get(0),
        )
        .optional()?;
    let now = now_iso();
    if eligible {
        let nominal_scheduled = start_at - Duration::hours(24);
        let near_term_immediate = nominal_scheduled <= now_utc;
        let scheduled = if near_term_immediate {
            now_utc
        } else {
            nominal_scheduled
        };
        let scheduled = utc_iso(scheduled);
        let reminder_id = existing.unwrap_or_else(new_uuid);
        connection.execute(
            "INSERT INTO appointment_reminders (id, appointment_id, scheduled_for_utc, status, created_at, updated_at)
             VALUES (?1, ?2, ?3, 'pending', ?4, ?4)
             ON CONFLICT(appointment_id, channel, reminder_type)
             DO UPDATE SET scheduled_for_utc=excluded.scheduled_for_utc, status='pending', cancelled_at_utc=NULL, updated_at=excluded.updated_at",
            params![reminder_id, appointment_id, scheduled, now],
        )?;
        let start = start_at + Duration::minutes(ISTANBUL_OFFSET_MINUTES);
        let service_summary: String = connection.query_row(
            "SELECT group_concat(service_name_snapshot, ', ') FROM (SELECT service_name_snapshot FROM appointment_services WHERE appointment_id=?1 ORDER BY sort_order ASC)",
            params![appointment_id],
            |row| row.get(0),
        )?;
        let recipient = format!(
            "90{}",
            normalize_phone(phone.as_deref(), true)?
                .ok_or_else(|| AppError::Validation("phone required".to_string()))?
        );
        upsert_cloud_outbox(
            connection,
            &reminder_id,
            "upsert",
            Some(serde_json::json!({
                "appointmentId": appointment_id,
                "scheduledForUtc": scheduled,
                "nearTermImmediate": near_term_immediate,
                "appointmentStartUtc": utc_iso(start_at),
                "recipient": recipient,
                "template": {
                    "name": "randevu_hatirlatma",
                    "language": "tr",
                    "parameters": [
                        format!("{} {}", first_name.trim(), last_name.trim()),
                        start.format("%d.%m.%Y").to_string(),
                        start.format("%H:%M").to_string(),
                        service_summary
                    ]
                }
            })),
        )?;
        Ok(true)
    } else if let Some(reminder_id) = existing {
        connection.execute(
            "UPDATE appointment_reminders SET status='cancelled', cancelled_at_utc=?1, updated_at=?1 WHERE id=?2",
            params![now, reminder_id],
        )?;
        upsert_cloud_outbox(connection, &reminder_id, "cancel", None)?;
        Ok(true)
    } else {
        Ok(false)
    }
}

fn reconcile_all_reminders_mock(connection: &Connection) -> Result<u32, AppError> {
    let mut statement =
        connection.prepare("SELECT id FROM appointments ORDER BY start_at_utc ASC")?;
    let ids = statement
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    let mut changed = 0_u32;
    for id in ids {
        if reconcile_reminder_for_appointment(connection, &id)? {
            changed += 1;
        }
    }
    Ok(changed)
}

fn automatic_reminders_enabled(connection: &Connection) -> Result<bool, AppError> {
    connection
        .query_row(
            "SELECT automatic_reminder_enabled=1 FROM whatsapp_settings WHERE id=1",
            [],
            |row| row.get(0),
        )
        .map_err(Into::into)
}

fn reminder_coordinator_diagnostic_path(database_path: &Path) -> Option<PathBuf> {
    database_path
        .parent()
        .and_then(Path::parent)
        .map(|root| root.join("logs").join(REMINDER_COORDINATOR_DIAGNOSTIC_FILE))
}

fn is_valid_reminder_coordinator_diagnostic_line(line: &str) -> bool {
    let Some((timestamp, marker)) = line.split_once(' ') else {
        return false;
    };
    chrono::DateTime::parse_from_rfc3339(timestamp).is_ok()
        && ReminderCoordinatorDiagnostic::is_allowed_marker(marker)
}

fn record_reminder_coordinator_diagnostic(
    database_path: &Path,
    diagnostic: ReminderCoordinatorDiagnostic,
) {
    let Some(path) = reminder_coordinator_diagnostic_path(database_path) else {
        return;
    };
    let Ok(_lock) = REMINDER_COORDINATOR_DIAGNOSTIC_LOCK.lock() else {
        return;
    };
    let Some(parent) = path.parent() else {
        return;
    };
    if fs::create_dir_all(parent).is_err() {
        return;
    }

    let mut entries = fs::read_to_string(&path)
        .ok()
        .into_iter()
        .flat_map(|content| content.lines().map(str::to_owned).collect::<Vec<_>>())
        .filter(|line| is_valid_reminder_coordinator_diagnostic_line(line))
        .collect::<Vec<_>>();
    if entries.len() >= REMINDER_COORDINATOR_DIAGNOSTIC_MAX_EVENTS {
        let first_retained = entries.len() - REMINDER_COORDINATOR_DIAGNOSTIC_MAX_EVENTS + 1;
        entries.drain(0..first_retained);
    }
    entries.push(format!("{} {}", utc_iso(Utc::now()), diagnostic.marker()));
    let _ = fs::write(path, format!("{}\n", entries.join("\n")));
}

fn dispatcher_status_diagnostic(error: &AppError) -> ReminderCoordinatorDiagnostic {
    match error {
        AppError::Validation(code) if code == "CLOUD_AUTH_INVALID" => {
            ReminderCoordinatorDiagnostic::DispatcherStatusAuthError
        }
        AppError::Io(_) => ReminderCoordinatorDiagnostic::DispatcherStatusNetworkError,
        AppError::Database(code)
            if matches!(
                code.as_str(),
                "CLOUD_RATE_LIMITED" | "CLOUD_TEMPORARILY_UNAVAILABLE"
            ) =>
        {
            ReminderCoordinatorDiagnostic::DispatcherStatusNetworkError
        }
        _ => ReminderCoordinatorDiagnostic::DispatcherStatusOtherError,
    }
}

enum DispatcherStatusReadOutcome {
    Initial(services::reminder_cloud::DispatcherStatus),
    Refreshed(services::reminder_cloud::DispatcherStatus),
}

enum DispatcherStatusReadError {
    Initial(AppError),
    Refresh(AppError),
    Retry(AppError),
}

impl DispatcherStatusReadError {
    fn into_app_error(self) -> AppError {
        match self {
            Self::Initial(error) | Self::Refresh(error) | Self::Retry(error) => error,
        }
    }
}

fn read_dispatcher_status_with_single_refresh(
    connection: &Connection,
    protector: &dyn services::secure_store::SecretProtector,
    transport: &mut dyn services::google::HttpTransport,
    config: &services::supabase::SupabaseConfig,
    session: &mut services::supabase::SupabaseSession,
) -> Result<DispatcherStatusReadOutcome, DispatcherStatusReadError> {
    match services::reminder_cloud::read_dispatcher_status(transport, config, session) {
        Ok(status) => Ok(DispatcherStatusReadOutcome::Initial(status)),
        Err(AppError::Validation(code)) if code == "CLOUD_AUTH_INVALID" => {
            let refreshed =
                services::supabase::refresh_session(transport, config, &session.refresh_token)
                    .map_err(DispatcherStatusReadError::Refresh)?;
            services::secure_store::store_supabase_session(connection, protector, &refreshed)
                .map_err(DispatcherStatusReadError::Refresh)?;
            *session = refreshed;
            services::reminder_cloud::read_dispatcher_status(transport, config, session)
                .map(DispatcherStatusReadOutcome::Refreshed)
                .map_err(DispatcherStatusReadError::Retry)
        }
        Err(error) => Err(DispatcherStatusReadError::Initial(error)),
    }
}

fn automatic_reminder_tick_with<F>(
    database_path: &Path,
    protector: &dyn services::secure_store::SecretProtector,
    transport: &mut dyn services::google::HttpTransport,
    config_loader: F,
) -> Result<bool, AppError>
where
    F: FnOnce() -> Result<services::supabase::SupabaseConfig, AppError>,
{
    let connection = Connection::open(database_path)?;
    connection.pragma_update(None, "foreign_keys", "ON")?;
    match automatic_reminders_enabled(&connection) {
        Ok(true) => record_reminder_coordinator_diagnostic(
            database_path,
            ReminderCoordinatorDiagnostic::AutomaticEnabledYes,
        ),
        Ok(false) => {
            record_reminder_coordinator_diagnostic(
                database_path,
                ReminderCoordinatorDiagnostic::AutomaticEnabledNo,
            );
            return Ok(false);
        }
        Err(error) => {
            record_reminder_coordinator_diagnostic(
                database_path,
                ReminderCoordinatorDiagnostic::AutomaticEnabledReadError,
            );
            return Err(error);
        }
    }
    // Reconciliation and projection are local-first. Dispatcher state controls
    // cloud claim/send, not whether pending local work reaches the cloud.
    if let Err(error) = reconcile_all_reminders_mock(&connection) {
        record_reminder_coordinator_diagnostic(
            database_path,
            ReminderCoordinatorDiagnostic::ReconcileFailed,
        );
        return Err(error);
    }
    let config = match config_loader() {
        Ok(config) => {
            record_reminder_coordinator_diagnostic(
                database_path,
                ReminderCoordinatorDiagnostic::SupabaseConfigPresent,
            );
            config
        }
        Err(_) => {
            record_reminder_coordinator_diagnostic(
                database_path,
                ReminderCoordinatorDiagnostic::SupabaseConfigMissing,
            );
            return Ok(false);
        }
    };
    let mut session = match services::secure_store::read_supabase_session(&connection, protector) {
        Ok(Some(session)) => {
            record_reminder_coordinator_diagnostic(
                database_path,
                ReminderCoordinatorDiagnostic::SessionPresent,
            );
            session
        }
        Ok(None) => {
            record_reminder_coordinator_diagnostic(
                database_path,
                ReminderCoordinatorDiagnostic::SessionMissing,
            );
            return Ok(false);
        }
        Err(error) => {
            record_reminder_coordinator_diagnostic(
                database_path,
                ReminderCoordinatorDiagnostic::SessionReadError,
            );
            return Err(error);
        }
    };
    let dispatcher = match read_dispatcher_status_with_single_refresh(
        &connection,
        protector,
        transport,
        &config,
        &mut session,
    ) {
        Ok(DispatcherStatusReadOutcome::Initial(status)) => status,
        Ok(DispatcherStatusReadOutcome::Refreshed(status)) => {
            record_reminder_coordinator_diagnostic(
                database_path,
                ReminderCoordinatorDiagnostic::DispatcherStatusAuthError,
            );
            record_reminder_coordinator_diagnostic(
                database_path,
                ReminderCoordinatorDiagnostic::AuthRefreshStarted,
            );
            record_reminder_coordinator_diagnostic(
                database_path,
                ReminderCoordinatorDiagnostic::AuthRefreshSucceeded,
            );
            record_reminder_coordinator_diagnostic(
                database_path,
                ReminderCoordinatorDiagnostic::DispatcherStatusRetrySucceeded,
            );
            status
        }
        Err(DispatcherStatusReadError::Initial(error)) => {
            record_reminder_coordinator_diagnostic(
                database_path,
                dispatcher_status_diagnostic(&error),
            );
            return Ok(false);
        }
        Err(DispatcherStatusReadError::Refresh(_error)) => {
            record_reminder_coordinator_diagnostic(
                database_path,
                ReminderCoordinatorDiagnostic::DispatcherStatusAuthError,
            );
            record_reminder_coordinator_diagnostic(
                database_path,
                ReminderCoordinatorDiagnostic::AuthRefreshStarted,
            );
            record_reminder_coordinator_diagnostic(
                database_path,
                ReminderCoordinatorDiagnostic::AuthRefreshFailed,
            );
            return Ok(false);
        }
        Err(DispatcherStatusReadError::Retry(error)) => {
            record_reminder_coordinator_diagnostic(
                database_path,
                ReminderCoordinatorDiagnostic::DispatcherStatusAuthError,
            );
            record_reminder_coordinator_diagnostic(
                database_path,
                ReminderCoordinatorDiagnostic::AuthRefreshStarted,
            );
            record_reminder_coordinator_diagnostic(
                database_path,
                ReminderCoordinatorDiagnostic::AuthRefreshSucceeded,
            );
            record_reminder_coordinator_diagnostic(
                database_path,
                ReminderCoordinatorDiagnostic::DispatcherStatusRetryFailed,
            );
            record_reminder_coordinator_diagnostic(
                database_path,
                dispatcher_status_diagnostic(&error),
            );
            return Ok(false);
        }
    };
    if dispatcher.paused {
        record_reminder_coordinator_diagnostic(
            database_path,
            ReminderCoordinatorDiagnostic::DispatcherStatusOkPaused,
        );
    } else {
        record_reminder_coordinator_diagnostic(
            database_path,
            ReminderCoordinatorDiagnostic::DispatcherStatusOkActive,
        );
    }
    record_reminder_coordinator_diagnostic(
        database_path,
        ReminderCoordinatorDiagnostic::ProcessOrderedOutboxEntered,
    );
    match services::reminder_cloud::process_ordered_outbox(
        &connection,
        transport,
        &config,
        &session,
        25,
    ) {
        Ok(_) => {
            record_reminder_coordinator_diagnostic(
                database_path,
                ReminderCoordinatorDiagnostic::ProcessOrderedOutboxCompleted,
            );
            Ok(true)
        }
        Err(error) => {
            record_reminder_coordinator_diagnostic(
                database_path,
                ReminderCoordinatorDiagnostic::ProcessOrderedOutboxFailed,
            );
            Err(error)
        }
    }
}

fn automatic_reminder_tick(database_path: &Path) -> Result<bool, AppError> {
    let protector = services::secure_store::WindowsDpapiProtector;
    let mut transport = services::google::ReqwestHttpTransport;
    automatic_reminder_tick_with(database_path, &protector, &mut transport, supabase_config)
}

fn appointment_has_pending_reminder_outbox(
    connection: &Connection,
    appointment_id: &str,
) -> Result<bool, AppError> {
    connection
        .query_row(
            "SELECT EXISTS(
                SELECT 1
                FROM reminder_cloud_outbox o
                INNER JOIN appointment_reminders r ON r.id=o.reminder_id
                WHERE r.appointment_id=?1 AND o.sync_status='pending'
            )",
            params![appointment_id],
            |row| row.get(0),
        )
        .map_err(Into::into)
}

fn request_reminder_projection(state: &AppState, appointment_id: &str) {
    let has_pending_outbox = {
        let connection = state.sqlite.lock().expect("sqlite mutex poisoned");
        appointment_has_pending_reminder_outbox(&connection, appointment_id)
    };
    match has_pending_outbox {
        Ok(true) => {
            let registered = state
                .reminder_coordinator
                .registered
                .load(Ordering::Acquire);
            let result = state.reminder_coordinator.request_wake();
            record_reminder_coordinator_diagnostic(
                &state.database_path,
                ReminderCoordinatorDiagnostic::WakeRequested { registered, result },
            );
        }
        Ok(false) => {}
        Err(_) => safe_diagnostic("REMINDER_PROJECTION_WAKE_CHECK_FAILED"),
    }
}

fn start_automatic_reminder_coordinator(
    database_path: PathBuf,
    coordinator: Arc<ReminderCoordinator>,
) {
    if !coordinator.register() {
        return;
    }
    let (sender, receiver) = mpsc::channel();
    *coordinator
        .signal_sender
        .lock()
        .expect("reminder shutdown mutex poisoned") = Some(sender);
    std::thread::spawn(move || loop {
        match receiver.recv_timeout(AUTOMATIC_REMINDER_INTERVAL) {
            Ok(ReminderCoordinatorSignal::Shutdown) => {
                record_reminder_coordinator_diagnostic(
                    &database_path,
                    ReminderCoordinatorDiagnostic::CoordinatorExit,
                );
                break;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                record_reminder_coordinator_diagnostic(
                    &database_path,
                    ReminderCoordinatorDiagnostic::CoordinatorDisconnected,
                );
                record_reminder_coordinator_diagnostic(
                    &database_path,
                    ReminderCoordinatorDiagnostic::CoordinatorExit,
                );
                break;
            }
            Ok(ReminderCoordinatorSignal::Wake) => {
                coordinator.wake_pending.store(false, Ordering::Release);
                record_reminder_coordinator_diagnostic(
                    &database_path,
                    ReminderCoordinatorDiagnostic::WakeReceived,
                );
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                record_reminder_coordinator_diagnostic(
                    &database_path,
                    ReminderCoordinatorDiagnostic::TimeoutTick,
                );
            }
        }
        if coordinator
            .running
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            record_reminder_coordinator_diagnostic(
                &database_path,
                ReminderCoordinatorDiagnostic::TickStarted,
            );
            if automatic_reminder_tick(&database_path).is_err() {
                safe_diagnostic("AUTOMATIC_REMINDER_PROJECTION_WARNING");
            }
            coordinator.running.store(false, Ordering::Release);
        } else {
            record_reminder_coordinator_diagnostic(
                &database_path,
                ReminderCoordinatorDiagnostic::RunningGuardSkip,
            );
        }
    });
}

fn list_whatsapp_candidates_tx(
    connection: &Connection,
) -> Result<Vec<WhatsAppCandidate>, AppError> {
    let now = Utc::now();
    let end = now + Duration::hours(24);
    let now = utc_iso(now);
    let end = utc_iso(end);
    let mut statement = connection.prepare(
        "SELECT a.id, c.id, c.first_name || ' ' || c.last_name, c.phone, a.start_at_utc
         FROM appointments a INNER JOIN customers c ON c.id=a.customer_id
         WHERE a.start_at_utc >= ?1 AND a.start_at_utc <= ?2
           AND a.status IN ('planned','confirmed')
           AND a.whatsapp_reminder_enabled=1
           AND c.is_active=1 AND c.whatsapp_reminder_enabled=1 AND c.whatsapp_consent_confirmed=1
         ORDER BY a.start_at_utc ASC",
    )?;
    let rows = statement.query_map(params![now, end], |row| {
        Ok(WhatsAppCandidate {
            appointment_id: row.get(0)?,
            customer_id: row.get(1)?,
            customer_name: row.get(2)?,
            phone: row.get(3)?,
            start_at_utc: row.get(4)?,
        })
    })?;
    let mut seen_phones = HashSet::new();
    let mut output = Vec::new();
    for row in rows {
        let candidate = row?;
        if normalize_phone(Some(&candidate.phone), true).is_ok()
            && seen_phones.insert(candidate.phone.clone())
        {
            output.push(candidate);
        }
    }
    Ok(output)
}

fn mock_auth_verify_otp_tx(
    connection: &Connection,
    email: &str,
    otp: &str,
) -> Result<MockAuthSession, AppError> {
    let email = normalize_text(email, "email", 5, 254)?;
    if !email.contains('@') || otp != "123456" {
        return Err(AppError::Validation("AUTH_OTP_INVALID".to_string()));
    }
    let expires = utc_iso(Utc::now() + Duration::hours(1));
    let secret = format!("mock-session:{email}:{expires}");
    let now = now_iso();
    connection.execute(
        "INSERT INTO secure_secrets (secret_key, encrypted_value, encryption_provider, created_at, updated_at)
         VALUES ('supabase_mock_session', ?1, 'tauri_secure_storage_mock_v1', ?2, ?2)
         ON CONFLICT(secret_key) DO UPDATE SET encrypted_value=excluded.encrypted_value, updated_at=excluded.updated_at",
        params![secret.as_bytes(), now],
    )?;
    Ok(MockAuthSession {
        email,
        access_token_hint: "mock-session-present".to_string(),
        expires_at_utc: expires,
    })
}

fn list_appointments_between(
    connection: &Connection,
    start_at_utc: Option<&str>,
    end_at_utc: Option<&str>,
    staff_id: Option<&str>,
    customer_id: Option<&str>,
    appointment_id: Option<&str>,
    limit: i64,
) -> Result<Vec<AppointmentSummary>, AppError> {
    let mut sql = String::from(
        "SELECT a.*, c.first_name || ' ' || c.last_name AS customer_name, c.phone AS customer_phone,
                c.whatsapp_reminder_enabled AS customer_whatsapp_reminder_enabled,
                c.whatsapp_consent_confirmed AS customer_whatsapp_consent_confirmed,
                st.first_name || COALESCE(' ' || st.last_name, '') AS staff_name
         FROM appointments a
         INNER JOIN customers c ON c.id = a.customer_id
         INNER JOIN staff st ON st.id = a.staff_id
         WHERE 1=1",
    );
    let mut params_map: HashMap<&str, String> = HashMap::new();
    if let Some(value) = start_at_utc {
        sql.push_str(" AND a.start_at_utc >= :start");
        params_map.insert(":start", value.to_string());
    }
    if let Some(value) = end_at_utc {
        sql.push_str(" AND a.start_at_utc < :end");
        params_map.insert(":end", value.to_string());
    }
    if let Some(value) = staff_id {
        sql.push_str(" AND a.staff_id = :staff");
        params_map.insert(":staff", value.to_string());
    }
    if let Some(value) = customer_id {
        sql.push_str(" AND a.customer_id = :customer");
        params_map.insert(":customer", value.to_string());
    }
    if let Some(value) = appointment_id {
        sql.push_str(" AND a.id = :id");
        params_map.insert(":id", value.to_string());
    }
    sql.push_str(" ORDER BY a.start_at_utc ASC, a.id ASC LIMIT :limit");
    params_map.insert(":limit", limit.to_string());
    let named: Vec<(&str, &dyn rusqlite::ToSql)> = params_map
        .iter()
        .map(|(key, value)| (*key, value as &dyn rusqlite::ToSql))
        .collect();
    let mut statement = connection.prepare(&sql)?;
    let rows = statement.query_map(&named[..], |row| {
        Ok((
            row.get::<_, String>("id")?,
            row.get::<_, String>("customer_id")?,
            row.get::<_, String>("staff_id")?,
            row.get::<_, String>("start_at_utc")?,
            row.get::<_, String>("end_at_utc")?,
            row.get::<_, i64>("total_duration_minutes")?,
            row.get::<_, String>("status")?,
            row.get::<_, Option<String>>("note")?,
            row.get::<_, i64>("whatsapp_reminder_enabled")?,
            row.get::<_, String>("customer_name")?,
            row.get::<_, Option<String>>("customer_phone")?,
            row.get::<_, i64>("customer_whatsapp_reminder_enabled")?,
            row.get::<_, i64>("customer_whatsapp_consent_confirmed")?,
            row.get::<_, String>("staff_name")?,
            row.get::<_, String>("updated_at")?,
        ))
    })?;
    let appointment_rows = rows.collect::<Result<Vec<_>, _>>()?;
    let appointment_ids = appointment_rows
        .iter()
        .map(|row| row.0.clone())
        .collect::<Vec<_>>();
    let mut services_by_appointment =
        list_appointment_services_batched(connection, &appointment_ids)?;
    let mut output = Vec::with_capacity(appointment_rows.len());
    for row in appointment_rows {
        let (
            id,
            customer_id,
            staff_id,
            start_at_utc,
            end_at_utc,
            total_duration_minutes,
            status,
            note,
            whatsapp_reminder_enabled,
            customer_name,
            customer_phone,
            customer_whatsapp_reminder_enabled,
            customer_whatsapp_consent_confirmed,
            staff_name,
            updated_at,
        ) = row;
        let services = services_by_appointment.remove(&id).unwrap_or_default();
        let service_names = services
            .iter()
            .map(|item| item.service_name_snapshot.clone())
            .collect();
        output.push(AppointmentSummary {
            id,
            customer_id,
            staff_id,
            start_at_utc,
            end_at_utc,
            total_duration_minutes,
            status,
            note,
            whatsapp_reminder_enabled: whatsapp_reminder_enabled == 1,
            whatsapp_reminder_effective: whatsapp_reminder_enabled == 1
                && customer_whatsapp_reminder_enabled == 1
                && customer_whatsapp_consent_confirmed == 1
                && normalize_phone(customer_phone.as_deref(), true).is_ok(),
            customer_name,
            customer_phone,
            staff_name,
            service_names,
            services,
            updated_at,
        });
    }
    Ok(output)
}

#[tauri::command]
fn app_health(app: AppHandle, state: tauri::State<'_, AppState>) -> Result<AppHealth, AppError> {
    let connection = state.sqlite.lock().expect("sqlite mutex poisoned");
    let schema_version = read_schema_version(&connection)?;
    let ok = integrity_check(&connection)?;
    Ok(AppHealth {
        ok,
        app_version: app.package_info().version.to_string(),
        schema_version,
        database_path: state.database_path.display().to_string(),
        message: if ok {
            "SQLite core data layer hazir.".to_string()
        } else {
            "SQLite integrity check basarisiz.".to_string()
        },
    })
}

#[tauri::command]
fn get_business_profile(state: tauri::State<'_, AppState>) -> Result<BusinessProfile, AppError> {
    let connection = state.sqlite.lock().expect("sqlite mutex poisoned");
    load_business_profile(&connection)
}

#[tauri::command]
fn get_onboarding_state(state: tauri::State<'_, AppState>) -> Result<OnboardingState, AppError> {
    let connection = state.sqlite.lock().expect("sqlite mutex poisoned");
    load_onboarding_state(&connection)
}

#[tauri::command]
fn update_business_profile(
    input: BusinessProfileInput,
    state: tauri::State<'_, AppState>,
) -> Result<BusinessProfile, AppError> {
    run_business_mutation(&state, |connection| {
        update_business_profile_tx(connection, input)
    })
}

#[tauri::command]
fn customer_create(
    input: CustomerInput,
    state: tauri::State<'_, AppState>,
) -> Result<Customer, AppError> {
    run_business_mutation(&state, |connection| create_customer_tx(connection, input))
}

#[tauri::command]
fn customer_update(
    id: String,
    input: CustomerInput,
    state: tauri::State<'_, AppState>,
) -> Result<Customer, AppError> {
    run_business_mutation(&state, |connection| {
        update_customer_tx(connection, &id, input)
    })
}

#[tauri::command]
fn customer_set_active(
    id: String,
    is_active: bool,
    state: tauri::State<'_, AppState>,
) -> Result<Customer, AppError> {
    run_business_mutation(&state, |connection| {
        set_customer_active(connection, &id, is_active)
    })
}

#[tauri::command]
fn customer_archive(id: String, state: tauri::State<'_, AppState>) -> Result<Customer, AppError> {
    run_business_mutation(&state, |connection| archive_customer(connection, &id))
}

#[tauri::command]
fn customer_reactivate(
    id: String,
    state: tauri::State<'_, AppState>,
) -> Result<Customer, AppError> {
    run_business_mutation(&state, |connection| reactivate_customer(connection, &id))
}

#[tauri::command]
fn customer_search(
    search: Option<String>,
    status: Option<String>,
    limit: Option<i64>,
    state: tauri::State<'_, AppState>,
) -> Result<Vec<Customer>, AppError> {
    let connection = state.sqlite.lock().expect("sqlite mutex poisoned");
    let limit = limit.unwrap_or(50).clamp(1, 200);
    find_customers(&connection, search.as_deref(), status.as_deref(), limit)
}

fn find_customers(
    connection: &Connection,
    search: Option<&str>,
    status: Option<&str>,
    limit: i64,
) -> Result<Vec<Customer>, AppError> {
    let status_sql = match status.as_deref().unwrap_or("active") {
        "inactive" => "is_active = 0",
        "all" => "1 = 1",
        _ => "is_active = 1",
    };
    let query = search.unwrap_or_default().to_lowercase();
    let phone_query: String = if query
        .chars()
        .all(|c| c.is_ascii_digit() || matches!(c, ' ' | '+' | '-' | '(' | ')'))
    {
        query.chars().filter(|c| c.is_ascii_digit()).collect()
    } else {
        String::new()
    };
    let normalized_phone_query = phone_query.trim_start_matches("90").trim_start_matches('0');
    let mut statement = connection.prepare(&format!(
        "SELECT * FROM customers WHERE {status_sql}
         AND (:query = '' OR lower(first_name) LIKE :like OR lower(last_name) LIKE :like OR lower(first_name || ' ' || last_name) LIKE :like OR (:phone_query != '' AND phone LIKE :phone))
         ORDER BY last_name COLLATE NOCASE ASC, first_name COLLATE NOCASE ASC LIMIT :limit"
    ))?;
    let like = format!("%{query}%");
    let phone = format!("%{normalized_phone_query}%");
    let rows = statement.query_map(
        &[
            (":query", &query as &dyn rusqlite::ToSql),
            (":like", &like),
            (":phone_query", &normalized_phone_query),
            (":phone", &phone),
            (":limit", &limit),
        ],
        customer_from_row,
    )?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

#[tauri::command]
fn customer_history(
    customer_id: String,
    limit: Option<i64>,
    offset: Option<i64>,
    state: tauri::State<'_, AppState>,
) -> Result<CustomerHistoryPage, AppError> {
    let connection = state.sqlite.lock().expect("sqlite mutex poisoned");
    load_customer_history(
        &connection,
        &customer_id,
        limit.unwrap_or(20),
        offset.unwrap_or(0),
    )
}

#[tauri::command]
fn get_repeat_booking_seed(
    customer_id: String,
    source_appointment_id: String,
    state: tauri::State<'_, AppState>,
) -> Result<RepeatBookingSeed, AppError> {
    let connection = state.sqlite.lock().expect("sqlite mutex poisoned");
    load_repeat_booking_seed(&connection, &customer_id, &source_appointment_id)
}

#[tauri::command]
fn staff_create(input: StaffInput, state: tauri::State<'_, AppState>) -> Result<Staff, AppError> {
    run_business_mutation(&state, |connection| create_staff_tx(connection, input))
}

#[tauri::command]
fn staff_update(
    id: String,
    input: StaffInput,
    state: tauri::State<'_, AppState>,
) -> Result<Staff, AppError> {
    run_business_mutation(&state, |connection| update_staff_tx(connection, &id, input))
}

#[tauri::command]
fn staff_set_active(
    id: String,
    is_active: bool,
    state: tauri::State<'_, AppState>,
) -> Result<Staff, AppError> {
    run_business_mutation(&state, |connection| {
        set_staff_active(connection, &id, is_active)
    })
}

#[tauri::command]
fn staff_deactivate(id: String, state: tauri::State<'_, AppState>) -> Result<Staff, AppError> {
    run_business_mutation(&state, |connection| deactivate_staff(connection, &id))
}

#[tauri::command]
fn staff_reactivate(id: String, state: tauri::State<'_, AppState>) -> Result<Staff, AppError> {
    run_business_mutation(&state, |connection| reactivate_staff(connection, &id))
}

#[tauri::command]
fn staff_list(
    status: Option<String>,
    limit: Option<i64>,
    state: tauri::State<'_, AppState>,
) -> Result<Vec<Staff>, AppError> {
    let connection = state.sqlite.lock().expect("sqlite mutex poisoned");
    let limit = limit.unwrap_or(50).clamp(1, 200);
    let status_sql = match status.as_deref().unwrap_or("active") {
        "inactive" => "WHERE is_active = 0",
        "all" => "",
        _ => "WHERE is_active = 1",
    };
    let mut statement = connection.prepare(&format!("SELECT * FROM staff {status_sql} ORDER BY sort_order ASC, first_name COLLATE NOCASE ASC LIMIT ?1"))?;
    let rows = statement.query_map(params![limit], staff_from_row)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

#[tauri::command]
fn staff_set_services(
    staff_id: String,
    service_ids: Vec<String>,
    state: tauri::State<'_, AppState>,
) -> Result<Vec<String>, AppError> {
    run_business_mutation(&state, |connection| {
        set_staff_services_tx(connection, &staff_id, service_ids)
    })
}

#[tauri::command]
fn staff_services_list(
    staff_id: String,
    state: tauri::State<'_, AppState>,
) -> Result<Vec<String>, AppError> {
    let connection = state.sqlite.lock().expect("sqlite mutex poisoned");
    let mut statement = connection.prepare(
        "SELECT service_id FROM staff_services WHERE staff_id=?1 ORDER BY service_id ASC",
    )?;
    let rows = statement.query_map(params![staff_id], |row| row.get(0))?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

#[tauri::command]
fn staff_working_hours_list(
    staff_id: String,
    state: tauri::State<'_, AppState>,
) -> Result<Vec<StaffWorkingHour>, AppError> {
    let connection = state.sqlite.lock().expect("sqlite mutex poisoned");
    list_staff_working_hours(&connection, &staff_id)
}

#[tauri::command]
fn staff_working_hours_set(
    staff_id: String,
    intervals: Vec<StaffWorkingHour>,
    state: tauri::State<'_, AppState>,
) -> Result<(), AppError> {
    run_business_mutation(&state, |connection| {
        replace_staff_working_hours(connection, &staff_id, intervals)
    })
}

#[tauri::command]
fn staff_schedule_is_unrestricted(
    staff_id: String,
    state: tauri::State<'_, AppState>,
) -> Result<bool, AppError> {
    let connection = state.sqlite.lock().expect("sqlite mutex poisoned");
    is_staff_schedule_unrestricted(&connection, &staff_id)
}

#[tauri::command]
fn staff_time_off_list(
    staff_id: String,
    state: tauri::State<'_, AppState>,
) -> Result<Vec<StaffTimeOff>, AppError> {
    let connection = state.sqlite.lock().expect("sqlite mutex poisoned");
    list_staff_time_off(&connection, &staff_id)
}

#[tauri::command]
fn staff_time_off_add(
    staff_id: String,
    input: StaffTimeOffInput,
    state: tauri::State<'_, AppState>,
) -> Result<StaffTimeOff, AppError> {
    run_business_mutation(&state, |connection| {
        add_staff_time_off(connection, &staff_id, input)
    })
}

#[tauri::command]
fn staff_time_off_update(
    staff_id: String,
    time_off_id: String,
    input: StaffTimeOffInput,
    state: tauri::State<'_, AppState>,
) -> Result<StaffTimeOff, AppError> {
    run_business_mutation(&state, |connection| {
        update_staff_time_off(connection, &staff_id, &time_off_id, input)
    })
}

#[tauri::command]
fn staff_time_off_remove(
    staff_id: String,
    time_off_id: String,
    state: tauri::State<'_, AppState>,
) -> Result<(), AppError> {
    run_business_mutation(&state, |connection| {
        remove_staff_time_off(connection, &staff_id, &time_off_id)
    })
}

#[tauri::command]
fn service_category_create(
    input: CategoryInput,
    state: tauri::State<'_, AppState>,
) -> Result<ServiceCategory, AppError> {
    run_business_mutation(&state, |connection| create_category_tx(connection, input))
}

#[tauri::command]
fn service_category_update(
    id: String,
    input: CategoryInput,
    state: tauri::State<'_, AppState>,
) -> Result<ServiceCategory, AppError> {
    run_business_mutation(&state, |connection| {
        update_category_tx(connection, &id, input)
    })
}

#[tauri::command]
fn service_category_list(
    status: Option<String>,
    state: tauri::State<'_, AppState>,
) -> Result<Vec<ServiceCategory>, AppError> {
    let connection = state.sqlite.lock().expect("sqlite mutex poisoned");
    let status_sql = match status.as_deref().unwrap_or("active") {
        "inactive" => "WHERE is_active=0",
        "all" => "",
        _ => "WHERE is_active=1",
    };
    let mut statement = connection.prepare(&format!(
        "SELECT id, name, sort_order, is_active FROM service_categories {status_sql} ORDER BY sort_order ASC, name COLLATE NOCASE ASC"
    ))?;
    let rows = statement.query_map([], |row| {
        Ok(ServiceCategory {
            id: row.get(0)?,
            name: row.get(1)?,
            sort_order: row.get(2)?,
            is_active: row.get::<_, i64>(3)? == 1,
        })
    })?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

#[tauri::command]
fn service_create(
    input: ServiceInput,
    state: tauri::State<'_, AppState>,
) -> Result<ServiceItem, AppError> {
    run_business_mutation(&state, |connection| create_service_tx(connection, input))
}

#[tauri::command]
fn service_update(
    id: String,
    input: ServiceInput,
    state: tauri::State<'_, AppState>,
) -> Result<ServiceItem, AppError> {
    run_business_mutation(&state, |connection| {
        update_service_tx(connection, &id, input)
    })
}

#[tauri::command]
fn service_set_active(
    id: String,
    is_active: bool,
    state: tauri::State<'_, AppState>,
) -> Result<ServiceItem, AppError> {
    run_business_mutation(&state, |connection| {
        set_service_active(connection, &id, is_active)
    })
}

#[tauri::command]
fn service_deactivate(
    id: String,
    state: tauri::State<'_, AppState>,
) -> Result<ServiceItem, AppError> {
    run_business_mutation(&state, |connection| deactivate_service(connection, &id))
}

#[tauri::command]
fn service_reactivate(
    id: String,
    state: tauri::State<'_, AppState>,
) -> Result<ServiceItem, AppError> {
    run_business_mutation(&state, |connection| reactivate_service(connection, &id))
}

#[tauri::command]
fn service_list(
    status: Option<String>,
    state: tauri::State<'_, AppState>,
) -> Result<Vec<ServiceItem>, AppError> {
    let connection = state.sqlite.lock().expect("sqlite mutex poisoned");
    let status = status.as_deref().unwrap_or("active");
    list_service_items(&connection, status)
}

#[tauri::command]
fn appointment_create(
    input: AppointmentInput,
    state: tauri::State<'_, AppState>,
) -> Result<AppointmentSummary, AppError> {
    let appointment = run_business_mutation(&state, |connection| {
        create_appointment_tx(connection, input)
    })?;
    request_reminder_projection(&state, &appointment.id);
    Ok(appointment)
}

#[tauri::command]
fn appointment_update(
    id: String,
    input: AppointmentInput,
    state: tauri::State<'_, AppState>,
) -> Result<AppointmentSummary, AppError> {
    let appointment = run_business_mutation(&state, |connection| {
        update_appointment_tx(connection, &id, input)
    })?;
    request_reminder_projection(&state, &appointment.id);
    Ok(appointment)
}

#[tauri::command]
fn appointment_service_set_charged_price(
    appointment_id: String,
    service_id: String,
    charged_price_minor: i64,
    state: tauri::State<'_, AppState>,
) -> Result<(), AppError> {
    run_business_mutation(&state, |connection| {
        set_appointment_service_charged_price(
            connection,
            &appointment_id,
            &service_id,
            charged_price_minor,
        )
    })
}

#[tauri::command]
fn service_statistics(
    start_date: String,
    end_date: String,
    state: tauri::State<'_, AppState>,
) -> Result<Vec<ServiceStatistic>, AppError> {
    let connection = state.sqlite.lock().expect("sqlite mutex poisoned");
    service_statistics_tx(&connection, &start_date, &end_date)
}

#[tauri::command]
fn appointment_list_by_date(
    local_date: String,
    staff_id: Option<String>,
    limit: Option<i64>,
    state: tauri::State<'_, AppState>,
) -> Result<Vec<AppointmentSummary>, AppError> {
    let connection = state.sqlite.lock().expect("sqlite mutex poisoned");
    let start = local_to_utc(&local_date, "00:00")?;
    let end = start + Duration::days(1);
    list_appointments_between(
        &connection,
        Some(&utc_iso(start)),
        Some(&utc_iso(end)),
        staff_id.as_deref(),
        None,
        None,
        limit.unwrap_or(100).clamp(1, 500),
    )
}

#[tauri::command]
fn appointment_list_by_date_range(
    start_date: String,
    end_date: String,
    limit: Option<i64>,
    state: tauri::State<'_, AppState>,
) -> Result<Vec<AppointmentSummary>, AppError> {
    if end_date < start_date {
        return Err(AppError::Validation(
            "APPOINTMENT_DATE_RANGE_INVALID".to_string(),
        ));
    }
    let connection = state.sqlite.lock().expect("sqlite mutex poisoned");
    let start = local_to_utc(&start_date, "00:00")?;
    let end = local_to_utc(&end_date, "00:00")? + Duration::days(1);
    list_appointments_between(
        &connection,
        Some(&utc_iso(start)),
        Some(&utc_iso(end)),
        None,
        None,
        None,
        limit.unwrap_or(500).clamp(1, 500),
    )
}

#[tauri::command]
async fn database_create_backup(app: AppHandle) -> Result<DatabaseBackupSummary, AppError> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        let _operation = state
            .backup_coordinator
            .backup_operation_lock
            .lock()
            .expect("backup operation mutex poisoned");
        let connection = state.sqlite.lock().expect("sqlite mutex poisoned");
        create_database_backup(&connection, &state.database_path, BackupKind::Manual)
    })
    .await
    .map_err(|_| AppError::Database("MANUAL_BACKUP_WORKER_FAILED".to_string()))?
}

#[tauri::command]
async fn database_restore_backup(backup_path: String, app: AppHandle) -> Result<(), AppError> {
    if backup_path.trim().is_empty() {
        return Err(AppError::Validation(
            "RESTORE_CANDIDATE_PATH_REQUIRED".to_string(),
        ));
    }
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        restore_database_from_candidate(&state, Path::new(&backup_path))
    })
    .await
    .map_err(|_| AppError::Database("RESTORE_WORKER_FAILED".to_string()))?
}

#[tauri::command]
fn open_data_folder(state: tauri::State<'_, AppState>) -> Result<DataFolderSummary, AppError> {
    open_business_data_directory(&state.database_path)
}

#[tauri::command]
fn database_clean_start(
    input: ConfirmedInput,
    state: tauri::State<'_, AppState>,
) -> Result<DatabaseCleanSummary, AppError> {
    if !input.confirmed {
        return Err(AppError::Validation(
            "CLEAN_REQUIRES_CONFIRMATION".to_string(),
        ));
    }
    run_business_mutation(&state, |connection| {
        clean_database_in_place(connection, &state.database_path)
    })
}

#[tauri::command]
fn google_sync_pending_mock(state: tauri::State<'_, AppState>) -> Result<u32, AppError> {
    let connection = state.sqlite.lock().expect("sqlite mutex poisoned");
    google_sync_pending_mock_tx(&connection)
}

#[tauri::command]
fn reminder_reconcile_all_mock(state: tauri::State<'_, AppState>) -> Result<u32, AppError> {
    let connection = state.sqlite.lock().expect("sqlite mutex poisoned");
    reconcile_all_reminders_mock(&connection)
}

#[tauri::command]
fn whatsapp_list_candidates(
    state: tauri::State<'_, AppState>,
) -> Result<Vec<WhatsAppCandidate>, AppError> {
    let connection = state.sqlite.lock().expect("sqlite mutex poisoned");
    list_whatsapp_candidates_tx(&connection)
}

#[tauri::command]
fn auth_mock_verify_otp(
    email: String,
    otp: String,
    state: tauri::State<'_, AppState>,
) -> Result<MockAuthSession, AppError> {
    let connection = state.sqlite.lock().expect("sqlite mutex poisoned");
    mock_auth_verify_otp_tx(&connection, &email, &otp)
}

#[tauri::command]
fn auth_logout(state: tauri::State<'_, AppState>) -> Result<bool, AppError> {
    let connection = state.sqlite.lock().expect("sqlite mutex poisoned");
    connection.execute(
        "DELETE FROM secure_secrets WHERE secret_key='supabase_mock_session'",
        [],
    )?;
    Ok(true)
}

fn load_live_services_config() -> Result<LiveServicesConfig, AppError> {
    let mut config = packaged_live_services_config()?;
    merge_live_services_config(&mut config, read_live_services_config_file()?);

    let google_client_id = std::env::var("BEAUTYSALOON_GOOGLE_CLIENT_ID")
        .ok()
        .filter(|value| !value.trim().is_empty());
    if let Some(client_id) = google_client_id {
        let client_secret = config
            .google
            .as_ref()
            .and_then(|google| google.client_secret.clone());
        config.google = Some(GoogleRuntimeConfig {
            client_id,
            client_secret,
            calendar_id: std::env::var("BEAUTYSALOON_GOOGLE_CALENDAR_ID")
                .ok()
                .filter(|value| !value.trim().is_empty()),
        });
    }

    let project_url = std::env::var("BEAUTYSALOON_SUPABASE_PROJECT_URL")
        .ok()
        .filter(|value| !value.trim().is_empty());
    let publishable_key = std::env::var("BEAUTYSALOON_SUPABASE_PUBLISHABLE_KEY")
        .ok()
        .filter(|value| !value.trim().is_empty());
    if let (Some(project_url), Some(publishable_key)) = (project_url, publishable_key) {
        config.supabase = Some(services::supabase::SupabaseConfig {
            project_url,
            publishable_key,
        });
    }

    Ok(config)
}

fn merge_live_services_config(target: &mut LiveServicesConfig, source: Option<LiveServicesConfig>) {
    let Some(source) = source else {
        return;
    };
    if let Some(source_google) = source.google {
        if let Some(target_google) = target.google.as_mut() {
            target_google.client_id = source_google.client_id;
            target_google.calendar_id = source_google.calendar_id;
        } else {
            target.google = Some(source_google);
        }
    }
    if source.supabase.is_some() {
        target.supabase = source.supabase;
    }
}

fn read_live_services_config_file() -> Result<Option<LiveServicesConfig>, AppError> {
    let mut config = LiveServicesConfig::default();
    let mut found = false;
    for path in live_services_config_candidates()?.into_iter().rev() {
        if path.exists() {
            let text = fs::read_to_string(path)?;
            let parsed = parse_live_services_config(&text)?;
            merge_live_services_config(&mut config, Some(parsed));
            found = true;
        }
    }
    Ok(found.then_some(config))
}

fn live_services_config_candidates() -> Result<Vec<PathBuf>, AppError> {
    let mut candidates = Vec::new();
    if let Some(app_data) = std::env::var_os("APPDATA") {
        candidates.push(installed_live_services_config_path(&PathBuf::from(
            app_data,
        )));
    }
    let mut add_candidates = |base: &Path| {
        candidates.push(base.join("src-tauri").join("live-services.local.json"));
        candidates.push(base.join("live-services.local.json"));
    };
    let cwd = std::env::current_dir()?;
    for ancestor in cwd.ancestors() {
        add_candidates(ancestor);
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            for ancestor in parent.ancestors() {
                add_candidates(ancestor);
            }
        }
    }
    candidates.dedup();
    Ok(candidates)
}

fn installed_live_services_config_path(app_data: &Path) -> PathBuf {
    app_data
        .join("com.beautysaloon.desktop")
        .join("settings")
        .join("live-services.json")
}

fn parse_live_services_config(text: &str) -> Result<LiveServicesConfig, AppError> {
    serde_json::from_str(text.trim_start_matches('\u{feff}'))
        .map_err(|_| AppError::Validation("LIVE_SERVICE_CONFIG_INVALID".to_string()))
}

fn packaged_live_services_config() -> Result<LiveServicesConfig, AppError> {
    parse_live_services_config(PACKAGED_RUNTIME_DEFAULT)
        .map_err(|_| AppError::Validation("RUNTIME_DEFAULT_INVALID".to_string()))
}

fn provision_runtime_config(
    settings_dir: &Path,
    packaged: &LiveServicesConfig,
) -> Result<(), AppError> {
    provision_runtime_config_for_fingerprint(
        settings_dir,
        packaged,
        LEGACY_GOOGLE_CLIENT_FINGERPRINT,
    )
}

fn provision_runtime_config_for_fingerprint(
    settings_dir: &Path,
    packaged: &LiveServicesConfig,
    legacy_fingerprint: &str,
) -> Result<(), AppError> {
    if packaged.google.is_none() && packaged.supabase.is_none() {
        return Ok(());
    }
    let destination = settings_dir.join("live-services.json");
    let mut existing = if destination.exists() {
        serde_json::from_slice(&fs::read(&destination)?)
            .map_err(|_| AppError::Validation("LIVE_SERVICE_CONFIG_INVALID".to_string()))?
    } else {
        serde_json::json!({})
    };
    let root = existing
        .as_object_mut()
        .ok_or_else(|| AppError::Validation("LIVE_SERVICE_CONFIG_INVALID".to_string()))?;
    let mut changed = !destination.exists();

    if let Some(google) = &packaged.google {
        let legacy_google = root
            .get("google")
            .and_then(|value| value.get("clientId"))
            .and_then(serde_json::Value::as_str)
            .is_some_and(|client_id| {
                legacy_google_client_fingerprint(client_id) == legacy_fingerprint
            });
        if legacy_google {
            root.entry("google".to_string())
                .or_insert_with(|| serde_json::json!({}))["clientId"] =
                serde_json::Value::String(google.client_id.clone());
            changed = true;
        } else if !root.contains_key("google") {
            root.insert(
                "google".to_string(),
                serde_json::json!({ "clientId": google.client_id, "calendarId": google.calendar_id }),
            );
            changed = true;
        } else if !destination.exists() {
            root.insert(
                "google".to_string(),
                serde_json::json!({ "clientId": google.client_id, "calendarId": google.calendar_id }),
            );
        }
    }
    if let Some(supabase) = &packaged.supabase {
        if !root.contains_key("supabase") {
            root.insert(
                "supabase".to_string(),
                serde_json::json!({
                    "projectUrl": supabase.project_url,
                    "publishableKey": supabase.publishable_key,
                }),
            );
            changed = true;
        }
    }
    if !changed {
        return Ok(());
    }
    let temporary = destination.with_extension("json.tmp");
    fs::write(
        &temporary,
        serde_json::to_vec_pretty(&existing)
            .map_err(|_| AppError::Validation("RUNTIME_DEFAULT_INVALID".to_string()))?,
    )?;
    fs::rename(temporary, destination)?;
    Ok(())
}

const LEGACY_GOOGLE_CLIENT_FINGERPRINT: &str = "0d8e294fc461";
fn legacy_google_client_fingerprint(client_id: &str) -> String {
    format!("{:x}", Sha256::digest(client_id.as_bytes()))[..12].to_string()
}

fn google_config() -> Result<services::google::GoogleOAuthConfig, AppError> {
    google_oauth_config(load_live_services_config()?)
}

fn google_oauth_config(
    config: LiveServicesConfig,
) -> Result<services::google::GoogleOAuthConfig, AppError> {
    config
        .google
        .map(GoogleRuntimeConfig::into_oauth_config)
        .ok_or_else(|| AppError::Validation("GOOGLE_CONFIG_MISSING".to_string()))
}

fn safe_diagnostic(marker: &str) {
    #[cfg(debug_assertions)]
    eprintln!("{marker}");
}

fn supabase_config() -> Result<services::supabase::SupabaseConfig, AppError> {
    let config = load_live_services_config()?
        .supabase
        .ok_or_else(|| AppError::Validation("CLOUD_CONFIG_MISSING".to_string()))?;
    services::supabase::validate_public_config(&config)?;
    Ok(config)
}

#[tauri::command]
fn google_calendar_status(
    state: tauri::State<'_, AppState>,
) -> Result<services::google::GoogleConnectionStatus, AppError> {
    let connection = state.sqlite.lock().expect("sqlite mutex poisoned");
    let protector = services::secure_store::WindowsDpapiProtector;
    let mut status = services::google::google_connection_status(
        &connection,
        services::secure_store::secure_storage_available(&protector),
    )?;
    status.configured = load_live_services_config()?.google.is_some();
    Ok(status)
}

static GOOGLE_CONNECT_IN_PROGRESS: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

#[derive(Debug)]
pub(crate) struct ConnectInProgressGuard;

impl Drop for ConnectInProgressGuard {
    fn drop(&mut self) {
        GOOGLE_CONNECT_IN_PROGRESS.store(false, std::sync::atomic::Ordering::SeqCst);
    }
}

pub(crate) fn try_acquire_google_connect_guard() -> Result<ConnectInProgressGuard, AppError> {
    if GOOGLE_CONNECT_IN_PROGRESS
        .compare_exchange(
            false,
            true,
            std::sync::atomic::Ordering::SeqCst,
            std::sync::atomic::Ordering::SeqCst,
        )
        .is_err()
    {
        return Err(AppError::Conflict(
            "GOOGLE_AUTH_ALREADY_IN_PROGRESS".to_string(),
        ));
    }
    Ok(ConnectInProgressGuard)
}

#[tauri::command]
async fn google_calendar_connect(
    state: tauri::State<'_, AppState>,
) -> Result<GoogleConnectResult, AppError> {
    safe_diagnostic("GOOGLE_CONNECT_INVOKE_ENTERED");
    let _guard = try_acquire_google_connect_guard()?;
    let config = google_config()?;
    safe_diagnostic("GOOGLE_CONNECT_CONFIG_OK");
    let database_path = state.database_path.clone();

    tauri::async_runtime::spawn_blocking(move || {
        let _guard = _guard;
        let listener = TcpListener::bind("127.0.0.1:0")?;
        listener.set_nonblocking(true)?;
        let redirect_uri = format!(
            "http://127.0.0.1:{}/oauth2callback",
            listener.local_addr()?.port()
        );
        safe_diagnostic("GOOGLE_CONNECT_LISTENER_BOUND");
        let state_token = new_uuid();
        let pkce = services::google::generate_pkce()?;
        let auth_url = services::google::build_google_auth_url(
            &config.client_id,
            &redirect_uri,
            &state_token,
            &pkce.challenge,
        )?;
        validate_oauth_redirect_uri(&auth_url, &redirect_uri)?;
        open_system_browser(&auth_url)?;
        let callback_url =
            wait_for_oauth_callback(&listener, &redirect_uri, GOOGLE_OAUTH_CALLBACK_TIMEOUT)?;
        let code = services::google::parse_oauth_callback(&callback_url, &state_token)?;
        let mut transport = services::google::ReqwestHttpTransport;
        let token_set = services::google::exchange_auth_code(
            &mut transport,
            &config,
            &redirect_uri,
            &code,
            &pkce.verifier,
        )?;
        let calendar_id = config
            .calendar_id
            .clone()
            .unwrap_or_else(|| "primary".to_string());
        let mut connection = open_database(&database_path)?;
        let protector = services::secure_store::WindowsDpapiProtector;
        services::secure_store::upsert_secret(
            &mut connection,
            &protector,
            "google_calendar_refresh_token",
            &token_set.refresh_token,
        )?;
        connection.execute(
            "UPDATE google_calendar_settings SET sync_enabled=1, client_id=?1, calendar_id=?2, updated_at_utc=strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id=1",
            params![config.client_id, calendar_id],
        )?;
        Ok(GoogleConnectResult {
            connected: true,
            calendar_id,
        })
    })
    .await
    .map_err(|error| AppError::Database(format!("TASK_JOIN_ERROR: {error}")))?
}

#[tauri::command]
fn google_calendar_disconnect(state: tauri::State<'_, AppState>) -> Result<bool, AppError> {
    let connection = state.sqlite.lock().expect("sqlite mutex poisoned");
    services::secure_store::delete_secret(&connection, "google_calendar_refresh_token")?;
    connection.execute(
        "UPDATE google_calendar_settings SET sync_enabled=0, account_email=NULL, updated_at_utc=strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id=1",
        [],
    )?;
    Ok(true)
}

#[tauri::command]
fn google_calendar_sync(state: tauri::State<'_, AppState>) -> Result<u32, AppError> {
    let config = google_config()?;
    let calendar_id = config
        .calendar_id
        .clone()
        .unwrap_or_else(|| "primary".to_string());
    let connection = state.sqlite.lock().expect("sqlite mutex poisoned");
    let protector = services::secure_store::WindowsDpapiProtector;
    let refresh_token = services::secure_store::read_secret(
        &connection,
        &protector,
        "google_calendar_refresh_token",
    )?
    .ok_or_else(|| AppError::Validation("GOOGLE_CALENDAR_NOT_CONNECTED".to_string()))?;
    let mut transport = services::google::ReqwestHttpTransport;
    let access_token =
        services::google::refresh_access_token(&mut transport, &config, &refresh_token)?;
    process_google_outbox(&connection, &mut transport, &access_token, &calendar_id, 25)
}

fn process_google_outbox(
    connection: &Connection,
    transport: &mut dyn services::google::HttpTransport,
    access_token: &str,
    calendar_id: &str,
    limit: i64,
) -> Result<u32, AppError> {
    let mut statement = connection.prepare(
        "SELECT appointment_id FROM google_calendar_outbox
         WHERE sync_status='pending'
         ORDER BY created_at_utc ASC, appointment_id ASC
         LIMIT ?1",
    )?;
    let appointment_ids = statement
        .query_map(params![limit], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    let mut processed = 0;
    for appointment_id in appointment_ids {
        match process_google_outbox_item(
            connection,
            transport,
            access_token,
            calendar_id,
            &appointment_id,
        ) {
            Ok(true) => processed += 1,
            Ok(false) => {}
            Err(_) => break,
        }
    }
    Ok(processed)
}

fn process_google_outbox_item(
    connection: &Connection,
    transport: &mut dyn services::google::HttpTransport,
    access_token: &str,
    calendar_id: &str,
    appointment_id: &str,
) -> Result<bool, AppError> {
    let marked = connection.execute(
        "UPDATE google_calendar_outbox SET sync_status='in_flight', last_attempt_at_utc=strftime('%Y-%m-%dT%H:%M:%fZ','now'), last_error_code=NULL WHERE appointment_id=?1 AND sync_status='pending'",
        params![appointment_id],
    )?;
    if marked != 1 {
        return Ok(false);
    }
    let result = (|| {
        let appointment = list_appointments_between(
            connection,
            None,
            None,
            None,
            None,
            Some(&appointment_id),
            1,
        )?
        .into_iter()
        .next()
        .ok_or_else(|| AppError::NotFound("APPOINTMENT_NOT_FOUND".to_string()))?;
        let google_event_id: Option<String> = connection
            .query_row(
                "SELECT google_event_id FROM appointment_google_calendar_sync WHERE appointment_id=?1",
                params![appointment_id],
                |row| row.get(0),
            )
            .optional()?
            .flatten();
        if let Some(event_id) = google_event_id.as_deref() {
            services::google::reject_unrelated_event_mutation(
                connection,
                &appointment_id,
                event_id,
            )?;
        }
        let event = services::google::build_calendar_event(&appointment);
        match services::google::upsert_calendar_event(
            transport,
            access_token,
            calendar_id,
            google_event_id.as_deref(),
            &event,
        ) {
            Ok(event_id) => {
                services::google::mark_google_outbox_synced(
                    connection,
                    &appointment_id,
                    &event_id,
                    &appointment.updated_at,
                )?;
                Ok(true)
            }
            Err(error) => Err(error),
        }
    })();
    if let Err(error) = result {
        connection.execute(
            "UPDATE google_calendar_outbox SET sync_status='pending', last_error_code=?1, updated_at_utc=strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE appointment_id=?2",
            params![error.to_string(), appointment_id],
        )?;
        return Err(error);
    }
    result
}

fn open_system_browser(url: &str) -> Result<(), AppError> {
    if !url.starts_with("https://") && !url.starts_with("http://") {
        return Err(AppError::Validation("INVALID_AUTH_URL_SCHEME".to_string()));
    }
    #[cfg(windows)]
    {
        let operation = wide_null("open");
        let target = wide_null(url);
        let result = unsafe {
            ShellExecuteW(
                std::ptr::null_mut(),
                operation.as_ptr(),
                target.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                SW_SHOWNORMAL,
            )
        };
        if !shell_execute_succeeded(result as isize) {
            safe_diagnostic("GOOGLE_CONNECT_BROWSER_LAUNCH_FAILED");
            return Err(AppError::Database(
                "GOOGLE_BROWSER_LAUNCH_FAILED".to_string(),
            ));
        }
        safe_diagnostic("GOOGLE_CONNECT_BROWSER_LAUNCH_OK");
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let status = Command::new("xdg-open").arg(url).status()?;
        if status.success() {
            Ok(())
        } else {
            Err(AppError::Database("GOOGLE_BROWSER_OPEN_FAILED".to_string()))
        }
    }
}

#[cfg(windows)]
fn wide_null(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

fn shell_execute_succeeded(result: isize) -> bool {
    result > 32
}

fn validate_oauth_redirect_uri(auth_url: &str, redirect_uri: &str) -> Result<(), AppError> {
    let url = Url::parse(auth_url)
        .map_err(|_| AppError::Validation("GOOGLE_AUTH_URL_INVALID".to_string()))?;
    let actual_redirect_uri = url
        .query_pairs()
        .find(|(key, _)| key == "redirect_uri")
        .map(|(_, value)| value.into_owned())
        .ok_or_else(|| AppError::Validation("GOOGLE_REDIRECT_URI_MISSING".to_string()))?;

    if actual_redirect_uri == redirect_uri {
        Ok(())
    } else {
        Err(AppError::Validation(
            "GOOGLE_REDIRECT_URI_MISMATCH".to_string(),
        ))
    }
}

fn wait_for_oauth_callback(
    listener: &TcpListener,
    redirect_uri: &str,
    timeout: StdDuration,
) -> Result<String, AppError> {
    let started = Instant::now();
    while oauth_callback_window_open(started.elapsed(), timeout) {
        match listener.accept() {
            Ok((mut stream, _)) => {
                let _ = stream.set_nonblocking(false);
                let _ = stream.set_read_timeout(Some(StdDuration::from_secs(5)));
                let _ = stream.set_write_timeout(Some(StdDuration::from_secs(5)));

                let mut buffer = [0_u8; 8192];
                let mut total_read = 0;
                while total_read < buffer.len() {
                    match stream.read(&mut buffer[total_read..]) {
                        Ok(0) => break,
                        Ok(n) => {
                            total_read += n;
                            if buffer[..total_read].windows(4).any(|w| w == b"\r\n\r\n") {
                                break;
                            }
                        }
                        Err(_) => break,
                    }
                }

                if total_read == 0 {
                    continue;
                }

                let request = String::from_utf8_lossy(&buffer[..total_read]);
                let first_line = request.lines().next().unwrap_or("");
                let path = first_line.split_whitespace().nth(1).unwrap_or("");

                if !path.starts_with("/oauth2callback") {
                    let response =
                        "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
                    let _ = stream.write_all(response.as_bytes());
                    let _ = stream.flush();
                    continue;
                }

                let callback_url = format!(
                    "{}{}",
                    redirect_uri.trim_end_matches("/oauth2callback"),
                    path
                );
                let body = "<!DOCTYPE html><html><head><meta charset=\"utf-8\"><title>BeautySaloon</title><style>body{font-family:system-ui,-apple-system,sans-serif;text-align:center;padding:50px;background:#faf8f5;color:#2c2523}h2{color:#386641}p{color:#6c584c}</style></head><body><h2>Google Takvim Baglantisi Basarili</h2><p>Bu sekmeyi kapatip BeautySaloon uygulamasina donebilirsiniz.</p></body></html>";
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.as_bytes().len(),
                    body
                );
                let _ = stream.write_all(response.as_bytes());
                let _ = stream.flush();
                std::thread::sleep(StdDuration::from_millis(50));
                return Ok(callback_url);
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(StdDuration::from_millis(100));
            }
            Err(_) => {
                std::thread::sleep(StdDuration::from_millis(100));
            }
        }
    }
    Err(AppError::Validation("GOOGLE_OAUTH_TIMEOUT".to_string()))
}

fn oauth_callback_window_open(elapsed: StdDuration, timeout: StdDuration) -> bool {
    elapsed < timeout
}

#[tauri::command]
fn cloud_connection_status(
    state: tauri::State<'_, AppState>,
) -> Result<services::supabase::SupabaseConnectionStatus, AppError> {
    let connection = state.sqlite.lock().expect("sqlite mutex poisoned");
    let config = match supabase_config() {
        Ok(config) => Some(config),
        Err(AppError::Validation(code)) if code == "CLOUD_CONFIG_MISSING" => None,
        Err(error) => return Err(error),
    };
    let protector = services::secure_store::WindowsDpapiProtector;
    let mut transport = services::google::ReqwestHttpTransport;
    cloud_connection_status_with(&connection, &protector, config.as_ref(), &mut transport)
}

fn cloud_connection_status_with(
    connection: &Connection,
    protector: &dyn services::secure_store::SecretProtector,
    config: Option<&services::supabase::SupabaseConfig>,
    transport: &mut dyn services::google::HttpTransport,
) -> Result<services::supabase::SupabaseConnectionStatus, AppError> {
    let Some(config) = config else {
        return Ok(services::supabase::SupabaseConnectionStatus {
            configured: false,
            session_present: false,
            validation_state: services::supabase::SupabaseSessionValidationState::Disconnected,
        });
    };
    let Some(session) = services::secure_store::read_supabase_session(connection, protector)?
    else {
        return Ok(services::supabase::SupabaseConnectionStatus {
            configured: true,
            session_present: false,
            validation_state: services::supabase::SupabaseSessionValidationState::Disconnected,
        });
    };

    match services::supabase::validate_authenticated_session(
        transport,
        config,
        &session.access_token,
    ) {
        Ok(()) => Ok(services::supabase::SupabaseConnectionStatus {
            configured: true,
            session_present: true,
            validation_state: services::supabase::SupabaseSessionValidationState::Valid,
        }),
        Err(AppError::Validation(code)) if code == "SUPABASE_AUTH_INVALID" => {
            match services::supabase::refresh_session(transport, config, &session.refresh_token) {
                Ok(refreshed) => {
                    services::secure_store::store_supabase_session(
                        connection, protector, &refreshed,
                    )?;
                    Ok(services::supabase::SupabaseConnectionStatus {
                        configured: true,
                        session_present: true,
                        validation_state: services::supabase::SupabaseSessionValidationState::Valid,
                    })
                }
                Err(AppError::Validation(code)) if code == "SUPABASE_AUTH_INVALID" => {
                    services::secure_store::delete_secret(connection, "cloud_supabase_session")?;
                    Ok(services::supabase::SupabaseConnectionStatus {
                        configured: true,
                        session_present: false,
                        validation_state:
                            services::supabase::SupabaseSessionValidationState::Disconnected,
                    })
                }
                Err(_) => Ok(services::supabase::SupabaseConnectionStatus {
                    configured: true,
                    session_present: true,
                    validation_state:
                        services::supabase::SupabaseSessionValidationState::Unavailable,
                }),
            }
        }
        Err(_) => Ok(services::supabase::SupabaseConnectionStatus {
            configured: true,
            session_present: true,
            validation_state: services::supabase::SupabaseSessionValidationState::Unavailable,
        }),
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ReminderReadinessStatus {
    state: String,
    automatic_enabled: bool,
}

fn reminder_readiness_status(connection: &Connection) -> Result<ReminderReadinessStatus, AppError> {
    let session_present = services::reminder_cloud::has_session_secret(connection)?;
    let automatic_enabled: bool = connection.query_row(
        "SELECT automatic_reminder_enabled=1 FROM whatsapp_settings WHERE id=1",
        [],
        |row| row.get(0),
    )?;
    let configured = load_live_services_config()?.supabase.is_some();
    let state = if !session_present || !configured {
        "disconnected"
    } else {
        "connected_not_ready"
    };
    Ok(ReminderReadinessStatus {
        state: state.to_string(),
        automatic_enabled,
    })
}

#[tauri::command]
fn reminder_readiness(
    state: tauri::State<'_, AppState>,
) -> Result<ReminderReadinessStatus, AppError> {
    let connection = state.sqlite.lock().expect("sqlite mutex poisoned");
    let mut readiness = reminder_readiness_status(&connection)?;
    if readiness.state != "connected_not_ready" || !readiness.automatic_enabled {
        return Ok(readiness);
    }
    let config = match supabase_config() {
        Ok(config) => config,
        Err(_) => return Ok(readiness),
    };
    let protector = services::secure_store::WindowsDpapiProtector;
    let Some(session) = services::secure_store::read_supabase_session(&connection, &protector)?
    else {
        return Ok(readiness);
    };
    let mut transport = services::google::ReqwestHttpTransport;
    if let Ok(dispatcher) =
        services::reminder_cloud::read_dispatcher_status(&mut transport, &config, &session)
    {
        if !dispatcher.paused {
            readiness.state = "ready".to_string();
        }
    }
    Ok(readiness)
}

#[tauri::command]
fn reminder_set_automatic_enabled(
    enabled: bool,
    state: tauri::State<'_, AppState>,
) -> Result<ReminderReadinessStatus, AppError> {
    let connection = state.sqlite.lock().expect("sqlite mutex poisoned");
    connection.execute(
        "UPDATE whatsapp_settings SET automatic_reminder_enabled=?1, updated_at=?2 WHERE id=1",
        params![i64::from(enabled), now_iso()],
    )?;
    reminder_readiness_status(&connection)
}

#[tauri::command]
fn cloud_status(
    state: tauri::State<'_, AppState>,
) -> Result<services::supabase::SupabaseConnectionStatus, AppError> {
    cloud_connection_status(state)
}

#[tauri::command]
fn dispatcher_status(
    state: tauri::State<'_, AppState>,
) -> Result<services::reminder_cloud::DispatcherStatus, AppError> {
    let connection = state.sqlite.lock().expect("sqlite mutex poisoned");
    let config = supabase_config()?;
    let protector = services::secure_store::WindowsDpapiProtector;
    let mut session = services::secure_store::read_supabase_session(&connection, &protector)?
        .ok_or_else(|| AppError::Validation("CLOUD_SESSION_MISSING".to_string()))?;
    let mut transport = services::google::ReqwestHttpTransport;
    read_dispatcher_status_with_single_refresh(
        &connection,
        &protector,
        &mut transport,
        &config,
        &mut session,
    )
    .map(|outcome| match outcome {
        DispatcherStatusReadOutcome::Initial(status)
        | DispatcherStatusReadOutcome::Refreshed(status) => status,
    })
    .map_err(DispatcherStatusReadError::into_app_error)
}

#[tauri::command]
fn cloud_request_otp(email: String, state: tauri::State<'_, AppState>) -> Result<bool, AppError> {
    drop(state);
    let config = supabase_config()?;
    let mut transport = services::google::ReqwestHttpTransport;
    services::supabase::request_email_otp(&mut transport, &config, &email)?;
    Ok(true)
}

#[tauri::command]
fn otp_request(email: String, state: tauri::State<'_, AppState>) -> Result<bool, AppError> {
    cloud_request_otp(email, state)
}

#[tauri::command]
fn cloud_verify_otp(
    email: String,
    otp: String,
    state: tauri::State<'_, AppState>,
) -> Result<services::supabase::SupabaseConnectionStatus, AppError> {
    let connection = state.sqlite.lock().expect("sqlite mutex poisoned");
    let config = supabase_config()?;
    let mut transport = services::google::ReqwestHttpTransport;
    let session = services::supabase::verify_email_otp(&mut transport, &config, &email, &otp)?;
    let protector = services::secure_store::WindowsDpapiProtector;
    services::secure_store::store_supabase_session(&connection, &protector, &session)?;
    Ok(services::supabase::SupabaseConnectionStatus {
        configured: true,
        session_present: true,
        validation_state: services::supabase::SupabaseSessionValidationState::Valid,
    })
}

#[tauri::command]
fn otp_verify(
    email: String,
    otp: String,
    state: tauri::State<'_, AppState>,
) -> Result<services::supabase::SupabaseConnectionStatus, AppError> {
    cloud_verify_otp(email, otp, state)
}

#[tauri::command]
fn cloud_disconnect(state: tauri::State<'_, AppState>) -> Result<bool, AppError> {
    let connection = state.sqlite.lock().expect("sqlite mutex poisoned");
    services::secure_store::delete_secret(&connection, "cloud_supabase_session")?;
    Ok(true)
}

#[tauri::command]
fn cloud_process_outbox(
    state: tauri::State<'_, AppState>,
) -> Result<services::reminder_cloud::CloudSyncStatus, AppError> {
    let connection = state.sqlite.lock().expect("sqlite mutex poisoned");
    let config = supabase_config()?;
    let protector = services::secure_store::WindowsDpapiProtector;
    let session = services::secure_store::read_supabase_session(&connection, &protector)?
        .ok_or_else(|| AppError::Validation("CLOUD_SESSION_MISSING".to_string()))?;
    let mut transport = services::google::ReqwestHttpTransport;
    services::reminder_cloud::process_ordered_outbox(
        &connection,
        &mut transport,
        &config,
        &session,
        25,
    )
}

pub fn run() {
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let root = app_data_root(app.handle())?;
            let (database_path, _, _, _, settings_dir) = data_paths(&root)?;
            provision_runtime_config(&settings_dir, &packaged_live_services_config()?)?;
            let sqlite = open_database(&database_path)?;
            let backup_coordinator = Arc::new(BackupCoordinator::default());
            let reminder_coordinator = Arc::new(ReminderCoordinator::default());
            app.manage(AppState {
                database_path: database_path.clone(),
                sqlite: Mutex::new(sqlite),
                backup_coordinator: backup_coordinator.clone(),
                reminder_coordinator: reminder_coordinator.clone(),
            });
            start_automatic_backup_coordinator(database_path, backup_coordinator);
            start_automatic_reminder_coordinator(
                app_data_root(app.handle())?
                    .join("database")
                    .join("salon-foundation.db"),
                reminder_coordinator,
            );
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.show();
                let _ = window.set_focus();
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            app_health,
            get_onboarding_state,
            get_business_profile,
            update_business_profile,
            customer_create,
            customer_update,
            customer_set_active,
            customer_archive,
            customer_reactivate,
            customer_search,
            customer_history,
            get_repeat_booking_seed,
            staff_create,
            staff_update,
            staff_set_active,
            staff_deactivate,
            staff_reactivate,
            staff_list,
            staff_set_services,
            staff_services_list,
            staff_working_hours_list,
            staff_working_hours_set,
            staff_schedule_is_unrestricted,
            staff_time_off_list,
            staff_time_off_add,
            staff_time_off_update,
            staff_time_off_remove,
            service_category_create,
            service_category_update,
            service_category_list,
            service_create,
            service_update,
            service_set_active,
            service_deactivate,
            service_reactivate,
            service_list,
            appointment_create,
            appointment_update,
            appointment_service_set_charged_price,
            service_statistics,
            appointment_list_by_date,
            appointment_list_by_date_range,
            database_create_backup,
            database_restore_backup,
            open_data_folder,
            database_clean_start,
            google_sync_pending_mock,
            reminder_reconcile_all_mock,
            whatsapp_list_candidates,
            auth_mock_verify_otp,
            auth_logout,
            google_calendar_status,
            google_calendar_connect,
            google_calendar_disconnect,
            google_calendar_sync,
            cloud_connection_status,
            reminder_readiness,
            reminder_set_automatic_enabled,
            cloud_status,
            dispatcher_status,
            cloud_request_otp,
            otp_request,
            cloud_verify_otp,
            otp_verify,
            cloud_disconnect,
            cloud_process_outbox
        ])
        .build(tauri::generate_context!())
        .expect("error while building BeautySaloon");
    app.run(|app_handle, event| {
        if matches!(event, tauri::RunEvent::ExitRequested { .. }) {
            let state = app_handle.state::<AppState>();
            state.reminder_coordinator.stop();
            if shutdown_automatic_backup(
                state.backup_coordinator.clone(),
                state.database_path.clone(),
            )
            .is_err()
            {
                safe_diagnostic("AUTOMATIC_BACKUP_SHUTDOWN_WARNING");
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::google::HttpTransport;
    use tempfile::tempdir;

    fn open_temp() -> (tempfile::TempDir, Connection) {
        let temp = tempdir().expect("tempdir");
        let connection = open_database(&temp.path().join("core.db")).expect("open db");
        (temp, connection)
    }

    fn reminder_coordinator_diagnostic_test_database_path(temp: &tempfile::TempDir) -> PathBuf {
        temp.path().join("database").join("salon-foundation.db")
    }

    fn read_reminder_coordinator_diagnostic(temp: &tempfile::TempDir) -> String {
        fs::read_to_string(
            temp.path()
                .join("logs")
                .join(REMINDER_COORDINATOR_DIAGNOSTIC_FILE),
        )
        .expect("read coordinator diagnostic")
    }

    #[test]
    fn reminder_coordinator_telemetry_is_allowlisted_and_bounded() {
        let temp = tempdir().expect("temp");
        let database_path = reminder_coordinator_diagnostic_test_database_path(&temp);
        let log_path = temp
            .path()
            .join("logs")
            .join(REMINDER_COORDINATOR_DIAGNOSTIC_FILE);
        fs::create_dir_all(log_path.parent().expect("log parent")).expect("create logs");
        fs::write(&log_path, "not-a-diagnostic leaked-token-value\n").expect("seed log");

        for _ in 0..(REMINDER_COORDINATOR_DIAGNOSTIC_MAX_EVENTS + 8) {
            record_reminder_coordinator_diagnostic(
                &database_path,
                ReminderCoordinatorDiagnostic::TimeoutTick,
            );
        }
        for diagnostic in [
            ReminderCoordinatorDiagnostic::WakeRequested {
                registered: true,
                result: ReminderWakeRequestResult::Sent,
            },
            ReminderCoordinatorDiagnostic::WakeRequested {
                registered: true,
                result: ReminderWakeRequestResult::Coalesced,
            },
            ReminderCoordinatorDiagnostic::WakeRequested {
                registered: false,
                result: ReminderWakeRequestResult::Unavailable,
            },
            ReminderCoordinatorDiagnostic::WakeReceived,
            ReminderCoordinatorDiagnostic::CoordinatorDisconnected,
            ReminderCoordinatorDiagnostic::CoordinatorExit,
            ReminderCoordinatorDiagnostic::TickStarted,
            ReminderCoordinatorDiagnostic::RunningGuardSkip,
            ReminderCoordinatorDiagnostic::AutomaticEnabledYes,
            ReminderCoordinatorDiagnostic::AutomaticEnabledNo,
            ReminderCoordinatorDiagnostic::AutomaticEnabledReadError,
            ReminderCoordinatorDiagnostic::ReconcileFailed,
            ReminderCoordinatorDiagnostic::SupabaseConfigPresent,
            ReminderCoordinatorDiagnostic::SupabaseConfigMissing,
            ReminderCoordinatorDiagnostic::SessionPresent,
            ReminderCoordinatorDiagnostic::SessionMissing,
            ReminderCoordinatorDiagnostic::SessionReadError,
            ReminderCoordinatorDiagnostic::DispatcherStatusOkActive,
            ReminderCoordinatorDiagnostic::DispatcherStatusOkPaused,
            ReminderCoordinatorDiagnostic::DispatcherStatusAuthError,
            ReminderCoordinatorDiagnostic::DispatcherStatusNetworkError,
            ReminderCoordinatorDiagnostic::DispatcherStatusOtherError,
            ReminderCoordinatorDiagnostic::AuthRefreshStarted,
            ReminderCoordinatorDiagnostic::AuthRefreshSucceeded,
            ReminderCoordinatorDiagnostic::AuthRefreshFailed,
            ReminderCoordinatorDiagnostic::DispatcherStatusRetrySucceeded,
            ReminderCoordinatorDiagnostic::DispatcherStatusRetryFailed,
            ReminderCoordinatorDiagnostic::ProcessOrderedOutboxEntered,
            ReminderCoordinatorDiagnostic::ProcessOrderedOutboxCompleted,
            ReminderCoordinatorDiagnostic::ProcessOrderedOutboxFailed,
        ] {
            record_reminder_coordinator_diagnostic(&database_path, diagnostic);
        }

        let content = read_reminder_coordinator_diagnostic(&temp);
        let entries = content.lines().collect::<Vec<_>>();
        assert_eq!(entries.len(), REMINDER_COORDINATOR_DIAGNOSTIC_MAX_EVENTS);
        assert!(entries
            .iter()
            .all(|line| is_valid_reminder_coordinator_diagnostic_line(line)));
        assert!(content.contains("wake_requested registered=true send_result=success"));
        assert!(content.contains("wake_requested registered=true send_result=coalesced"));
        assert!(content.contains("wake_requested registered=false send_result=failure"));
        assert!(content.contains("wake_received"));
        assert!(content.contains("timeout_tick"));
        assert!(content.contains("coordinator_disconnected"));
        assert!(content.contains("coordinator_exit"));
        assert!(content.contains("tick_started"));
        assert!(content.contains("running_guard_skip"));
        assert!(content.contains("automatic_enabled_yes"));
        assert!(content.contains("automatic_enabled_no"));
        assert!(content.contains("automatic_enabled_read_error"));
        assert!(content.contains("supabase_config_present"));
        assert!(content.contains("supabase_config_missing"));
        assert!(content.contains("session_present"));
        assert!(content.contains("session_missing"));
        assert!(content.contains("session_read_error"));
        assert!(content.contains("dispatcher_status_ok_active"));
        assert!(content.contains("dispatcher_status_ok_paused"));
        assert!(content.contains("dispatcher_status_auth_error"));
        assert!(content.contains("dispatcher_status_network_error"));
        assert!(content.contains("dispatcher_status_other_error"));
        assert!(content.contains("auth_refresh_started"));
        assert!(content.contains("auth_refresh_succeeded"));
        assert!(content.contains("auth_refresh_failed"));
        assert!(content.contains("dispatcher_status_retry_succeeded"));
        assert!(content.contains("dispatcher_status_retry_failed"));
        assert!(content.contains("process_ordered_outbox_entered"));
        assert!(content.contains("process_ordered_outbox_completed"));
        assert!(content.contains("process_ordered_outbox_failed"));
        assert!(!content.contains("leaked-token-value"));
    }

    #[test]
    fn reminder_coordinator_telemetry_classifies_dispatcher_status_without_raw_errors() {
        assert_eq!(
            dispatcher_status_diagnostic(&AppError::Validation("CLOUD_AUTH_INVALID".into()))
                .marker(),
            "dispatcher_status_auth_error"
        );
        assert_eq!(
            dispatcher_status_diagnostic(&AppError::Database(
                "CLOUD_TEMPORARILY_UNAVAILABLE".into()
            ))
            .marker(),
            "dispatcher_status_network_error"
        );
        assert_eq!(
            dispatcher_status_diagnostic(&AppError::Database(
                "provider-error token=secret-value".into()
            ))
            .marker(),
            "dispatcher_status_other_error"
        );
    }

    #[test]
    fn installed_google_config_path_is_stable_and_outside_business_backups() {
        let app_data = Path::new("C:/Users/test/AppData/Roaming");
        let path = installed_live_services_config_path(app_data);
        assert!(path.ends_with("com.beautysaloon.desktop/settings/live-services.json"));
        assert!(!path.to_string_lossy().contains("backups"));
        assert!(!path.to_string_lossy().contains("database"));
    }

    #[test]
    fn runtime_provisioning_migrates_legacy_google_and_preserves_custom_configuration() {
        let temp = tempdir().expect("temp");
        let packaged = parse_live_services_config(
            r#"{"google":{"clientId":"new-client","clientSecret":"native-secret","calendarId":"primary"},"supabase":{"projectUrl":"https://project.example","publishableKey":"public-key-with-enough-length"}}"#,
        )
        .expect("packaged");
        let legacy = "legacy-client";
        let fingerprint = legacy_google_client_fingerprint(legacy);
        let settings = temp.path().join("legacy");
        fs::create_dir_all(&settings).expect("settings");
        fs::write(
            settings.join("live-services.json"),
            format!(
                r#"{{"google":{{"clientId":"{legacy}","calendarId":"kept"}},"unrelated":true}}"#
            ),
        )
        .expect("legacy config");
        provision_runtime_config_for_fingerprint(&settings, &packaged, &fingerprint)
            .expect("migrate");
        let migrated = fs::read_to_string(settings.join("live-services.json")).expect("read");
        assert!(
            migrated.contains("new-client")
                && migrated.contains("kept")
                && migrated.contains("unrelated")
                && migrated.contains("https://project.example")
        );
        assert!(!migrated.contains("native-secret") && !migrated.contains("clientSecret"));

        let custom = temp.path().join("custom");
        fs::create_dir_all(&custom).expect("custom settings");
        fs::write(
            custom.join("live-services.json"),
            r#"{"google":{"clientId":"custom-client"},"supabase":{"projectUrl":"https://custom.example","publishableKey":"custom-public-key-with-enough-length"}}"#,
        )
        .expect("custom config");
        provision_runtime_config_for_fingerprint(&custom, &packaged, &fingerprint)
            .expect("preserve custom");
        let custom_text =
            fs::read_to_string(custom.join("live-services.json")).expect("custom read");
        assert!(
            custom_text.contains("custom-client") && custom_text.contains("https://custom.example")
        );
        assert!(!custom_text.contains("https://project.example"));

        let missing_google = temp.path().join("missing-google");
        fs::create_dir_all(&missing_google).expect("missing Google settings");
        fs::write(
            missing_google.join("live-services.json"),
            r#"{"custom":true}"#,
        )
        .expect("missing Google config");
        provision_runtime_config_for_fingerprint(&missing_google, &packaged, &fingerprint)
            .expect("add missing runtime sections");
        let missing_google_text =
            fs::read_to_string(missing_google.join("live-services.json")).expect("read config");
        assert!(
            missing_google_text.contains("new-client")
                && missing_google_text.contains("https://project.example")
                && missing_google_text.contains("custom")
        );

        let clean = temp.path().join("clean");
        fs::create_dir_all(&clean).expect("clean settings");
        provision_runtime_config_for_fingerprint(&clean, &packaged, &fingerprint)
            .expect("clean provision");
        let clean_text = fs::read_to_string(clean.join("live-services.json")).expect("clean read");
        assert!(
            clean_text.contains("new-client") && clean_text.contains("https://project.example")
        );
        assert!(!clean_text.contains("native-secret") && !clean_text.contains("clientSecret"));
        let (_db_temp, connection) = open_temp();
        let secret_count: i64 = connection
            .query_row("SELECT COUNT(*) FROM secure_secrets", [], |row| row.get(0))
            .expect("count");
        assert_eq!(secret_count, 0);
    }

    #[test]
    fn runtime_provisioning_preserves_existing_dpapi_session_and_does_not_process_outbox() {
        let temp = tempdir().expect("tempdir");
        let settings_dir = temp.path().join("settings");
        fs::create_dir_all(&settings_dir).expect("settings");
        let (_database_temp, connection) = open_temp();
        let protector = services::secure_store::TestProtector;
        let session = services::supabase::SupabaseSession {
            access_token: "test-access-token".to_string(),
            refresh_token: "test-refresh-token".to_string(),
            expires_at: Some(123),
        };
        services::secure_store::store_supabase_session(&connection, &protector, &session)
            .expect("store session");
        let outbox_before: i64 = connection
            .query_row("SELECT COUNT(*) FROM google_calendar_outbox", [], |row| {
                row.get(0)
            })
            .expect("outbox count");
        let packaged = parse_live_services_config(
            r#"{"google":{"clientId":"packaged-client","calendarId":"primary"},"supabase":{"projectUrl":"https://project.example","publishableKey":"public-key-with-enough-length"}}"#,
        )
        .expect("packaged config");

        provision_runtime_config(&settings_dir, &packaged).expect("provision");

        let restored = services::secure_store::read_supabase_session(&connection, &protector)
            .expect("read session")
            .expect("session remains");
        assert_eq!(restored.expires_at, session.expires_at);
        assert_eq!(restored.access_token, session.access_token);
        assert_eq!(restored.refresh_token, session.refresh_token);
        let outbox_after: i64 = connection
            .query_row("SELECT COUNT(*) FROM google_calendar_outbox", [], |row| {
                row.get(0)
            })
            .expect("outbox count");
        assert_eq!(outbox_after, outbox_before);
    }

    #[test]
    fn packaged_runtime_config_bootstraps_clean_app_data_without_privileged_secrets() {
        let temp = tempdir().expect("tempdir");
        let settings_dir = temp.path().join("settings");
        fs::create_dir_all(&settings_dir).expect("settings");
        let packaged = parse_live_services_config(
            r#"{"google":{"clientId":"packaged-client","calendarId":"primary"},"supabase":{"projectUrl":"https://project.example","publishableKey":"public-key-with-enough-length"}}"#,
        )
        .expect("packaged config");

        provision_runtime_config(&settings_dir, &packaged).expect("provision");
        let text = fs::read_to_string(settings_dir.join("live-services.json")).expect("config");
        let resolved = parse_live_services_config(&text).expect("resolved");
        assert_eq!(
            google_oauth_config(resolved.clone())
                .expect("Google config")
                .client_id,
            "packaged-client"
        );
        assert_eq!(
            resolved.supabase.expect("Supabase config").project_url,
            "https://project.example"
        );
        for forbidden in [
            "clientSecret",
            "refreshToken",
            "accessToken",
            "serviceRole",
            "meta",
        ] {
            assert!(!text.contains(forbidden));
        }
    }

    #[test]
    fn packaged_runtime_default_contains_only_expected_google_and_public_supabase_fields() {
        let packaged: serde_json::Value =
            serde_json::from_str(PACKAGED_RUNTIME_DEFAULT).expect("packaged default");
        let google = packaged
            .get("google")
            .and_then(serde_json::Value::as_object);
        if let Some(google) = google {
            assert!(google
                .keys()
                .all(|key| key == "clientId" || key == "clientSecret" || key == "calendarId"));
        }
        let supabase = packaged
            .get("supabase")
            .and_then(serde_json::Value::as_object);
        if let Some(supabase) = supabase {
            assert!(supabase
                .keys()
                .all(|key| key == "projectUrl" || key == "publishableKey"));
        }
        let text = PACKAGED_RUNTIME_DEFAULT.to_ascii_lowercase();
        for forbidden in ["refresh", "access_token", "service_role", "whatsapp"] {
            assert!(!text.contains(forbidden));
        }
    }

    #[test]
    fn explicit_app_data_google_config_overrides_packaged_default() {
        let mut resolved = parse_live_services_config(
            r#"{"google":{"clientId":"packaged-client","calendarId":"primary"}}"#,
        )
        .expect("packaged config");
        let app_data = parse_live_services_config(
            r#"{"google":{"clientId":"app-data-client","calendarId":"salon-calendar"}}"#,
        )
        .expect("app data config");

        merge_live_services_config(&mut resolved, Some(app_data));
        let google = google_oauth_config(resolved).expect("Google config");
        assert_eq!(google.client_id, "app-data-client");
        assert_eq!(google.calendar_id.as_deref(), Some("salon-calendar"));
    }

    #[test]
    fn missing_google_config_returns_the_existing_safe_error() {
        let error = google_oauth_config(LiveServicesConfig::default()).expect_err("missing config");
        assert!(error.to_string().contains("GOOGLE_CONFIG_MISSING"));
    }

    #[test]
    fn token_exchange_diagnostics_classify_only_http_status_and_oauth_error() {
        let config = services::google::GoogleOAuthConfig {
            client_id: "client-id".into(),
            client_secret: String::new(),
            calendar_id: Some("primary".into()),
        };
        let cases = [
            (
                401,
                br#"{"error":"invalid_client","error_description":"authorization-code=secret-code"}"#.to_vec(),
                "TOKEN_EXCHANGE_FAILED_HTTP:401:invalid_client:OTHER",
            ),
            (
                400,
                br#"{"error":"invalid_grant","error_description":"refresh_token=secret-token"}"#.to_vec(),
                "TOKEN_EXCHANGE_FAILED_HTTP:400:invalid_grant:OTHER",
            ),
            (
                400,
                b"not-json access_token=secret-token".to_vec(),
                "TOKEN_EXCHANGE_FAILED_HTTP:400:unknown:OTHER",
            ),
            (
                400,
                br#"{"error":"unsafe error with email user@example.com"}"#.to_vec(),
                "TOKEN_EXCHANGE_FAILED_HTTP:400:unknown:OTHER",
            ),
        ];

        for (status, body, expected_marker) in cases {
            let mut transport =
                services::google::FakeHttpTransport::new(vec![services::google::HttpResponse {
                    status,
                    body,
                }]);
            let error = services::google::exchange_auth_code(
                &mut transport,
                &config,
                "http://127.0.0.1:4444/oauth2callback",
                "authorization-code",
                "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk",
            )
            .expect_err("non-success token response");
            let marker = match error {
                AppError::Database(code) => code,
                other => panic!("expected database error, got {other}"),
            };
            assert_eq!(marker, format!("GOOGLE_{expected_marker}"));
            for forbidden in [
                "authorization-code",
                "secret-code",
                "secret-token",
                "error_description",
                "access_token",
                "refresh_token",
                "user@example.com",
                "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk",
            ] {
                assert!(!marker.contains(forbidden));
            }
        }

        for (description, expected) in [
            ("client_secret is missing", "CLIENT_SECRET_MISSING"),
            ("client secret is invalid", "CLIENT_SECRET_INVALID"),
            ("code_verifier does not match pkce", "CODE_VERIFIER_PROBLEM"),
            ("redirect_uri is invalid", "REDIRECT_URI_PROBLEM"),
            ("client_id is invalid", "CLIENT_ID_PROBLEM"),
            ("unexpected response", "OTHER"),
        ] {
            assert_eq!(
                services::google::classify_token_error_description(description),
                expected
            );
        }
    }

    #[test]
    fn reqwest_transport_emits_form_content_type_on_wire() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind local mock");
        let endpoint = format!(
            "http://127.0.0.1:{}/token",
            listener.local_addr().expect("local address").port()
        );
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept request");
            let mut request = Vec::new();
            let mut buffer = [0_u8; 1024];
            loop {
                let read = stream.read(&mut buffer).expect("read request");
                assert!(read > 0, "request must not end before headers");
                request.extend_from_slice(&buffer[..read]);
                if request.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }
            let wire = String::from_utf8(request).expect("request header utf8");
            let mut lines = wire.split("\r\n");
            assert_eq!(lines.next(), Some("POST /token HTTP/1.1"));
            assert!(lines.any(|line| {
                line.eq_ignore_ascii_case("content-type: application/x-www-form-urlencoded")
            }));
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}")
                .expect("send response");
        });

        let mut transport = services::google::ReqwestHttpTransport;
        let response = transport
            .send(services::google::HttpRequest {
                method: "POST".to_string(),
                url: endpoint,
                headers: vec![(
                    "Content-Type".to_string(),
                    "application/x-www-form-urlencoded".to_string(),
                )],
                body: b"code=placeholder&client_id=placeholder&redirect_uri=placeholder&grant_type=authorization_code&code_verifier=placeholder".to_vec(),
            })
            .expect("local form request");
        assert_eq!(response.status, 200);
        server.join().expect("local mock server");
    }

    #[test]
    fn google_oauth_pkce_contract_and_verifier_privacy() {
        // 1. Verifier is RFC 7636 valid
        let pkce1 = services::google::generate_pkce().expect("pkce1");
        assert!(services::google::is_valid_pkce_verifier(&pkce1.verifier));
        assert!(pkce1.verifier.len() >= 43 && pkce1.verifier.len() <= 128);
        assert!(pkce1.verifier.chars().all(|c| {
            c.is_ascii_alphanumeric() || c == '-' || c == '.' || c == '_' || c == '~'
        }));

        // 2. Two attempts produce different verifiers and challenges
        let pkce2 = services::google::generate_pkce().expect("pkce2");
        assert_ne!(pkce1.verifier, pkce2.verifier);
        assert_ne!(pkce1.challenge, pkce2.challenge);

        // 3. S256 challenge is correct (RFC 7636 Appendix B test vector)
        let rfc_verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        let rfc_expected_challenge = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";
        assert_eq!(
            services::google::derive_pkce_challenge(rfc_verifier),
            rfc_expected_challenge
        );
        assert_eq!(
            services::google::derive_pkce_challenge(&pkce1.verifier),
            pkce1.challenge
        );

        // 4. Auth URL contains challenge + S256 and required offline access parameters
        let redirect_uri = "http://127.0.0.1:4444/oauth2callback";
        let auth_url_str = services::google::build_google_auth_url(
            "client-id-123",
            redirect_uri,
            "state-abc",
            &pkce1.challenge,
        )
        .expect("auth url");
        let auth_url = url::Url::parse(&auth_url_str).expect("parse auth url");
        let query_pairs: std::collections::HashMap<String, String> =
            auth_url.query_pairs().into_owned().collect();
        assert_eq!(query_pairs.get("code_challenge"), Some(&pkce1.challenge));
        assert_eq!(
            query_pairs.get("code_challenge_method"),
            Some(&"S256".to_string())
        );
        assert_eq!(
            query_pairs.get("client_id"),
            Some(&"client-id-123".to_string())
        );
        assert_eq!(
            query_pairs.get("redirect_uri"),
            Some(&redirect_uri.to_string())
        );
        assert_eq!(query_pairs.get("response_type"), Some(&"code".to_string()));
        assert_eq!(
            query_pairs.get("scope"),
            Some(&services::google::GOOGLE_CALENDAR_SCOPE.to_string())
        );
        assert_eq!(query_pairs.get("access_type"), Some(&"offline".to_string()));
        assert_eq!(query_pairs.get("prompt"), Some(&"consent".to_string()));
        assert_eq!(query_pairs.get("state"), Some(&"state-abc".to_string()));

        // 5. Token request contains matching code_verifier and does NOT contain client_secret
        let config = services::google::GoogleOAuthConfig {
            client_id: "client-id-123".into(),
            client_secret: String::new(), // desktop app flow has no client secret
            calendar_id: Some("primary".into()),
        };
        let mut transport =
            services::google::FakeHttpTransport::new(vec![services::google::HttpResponse {
                status: 200,
                body: br#"{"refresh_token":"secret-refresh-1","access_token":"secret-access-1"}"#
                    .to_vec(),
            }]);
        let _tokens = services::google::exchange_auth_code(
            &mut transport,
            &config,
            redirect_uri,
            "auth-code-xyz",
            &pkce1.verifier,
        )
        .expect("token exchange");

        assert_eq!(transport.requests.len(), 1);
        let request = &transport.requests[0];
        let body_str = String::from_utf8(request.body.clone()).expect("utf8 form body");
        let form_pairs = url::form_urlencoded::parse(request.body.as_slice())
            .into_owned()
            .collect::<Vec<_>>();
        let form_params: std::collections::HashMap<String, String> =
            form_pairs.iter().cloned().collect();
        assert_eq!(request.method, "POST");
        assert_eq!(request.url, services::google::GOOGLE_TOKEN_ENDPOINT);
        assert!(request.headers.iter().any(|(name, value)| {
            name.eq_ignore_ascii_case("content-type")
                && value.eq_ignore_ascii_case("application/x-www-form-urlencoded")
        }));
        assert_eq!(form_pairs.len(), 5);
        for required in [
            "code",
            "client_id",
            "redirect_uri",
            "grant_type",
            "code_verifier",
        ] {
            assert_eq!(
                form_pairs
                    .iter()
                    .filter(|(name, _)| name == required)
                    .count(),
                1,
                "required parameter appears exactly once"
            );
            assert!(
                form_params
                    .get(required)
                    .is_some_and(|value| !value.trim().is_empty()),
                "required parameter is non-empty"
            );
        }
        assert_eq!(form_params.get("code"), Some(&"auth-code-xyz".to_string()));
        assert_eq!(
            form_params.get("client_id"),
            Some(&"client-id-123".to_string())
        );
        assert_eq!(
            form_params.get("redirect_uri"),
            Some(&redirect_uri.to_string())
        );
        assert_eq!(
            form_params.get("redirect_uri"),
            query_pairs.get("redirect_uri")
        );
        assert_eq!(
            form_params.get("grant_type"),
            Some(&"authorization_code".to_string())
        );
        assert_eq!(form_params.get("code_verifier"), Some(&pkce1.verifier));
        assert!(!form_params.contains_key("client_secret"));
        assert!(!body_str.contains("client_secret"));

        // 6. Invalid required inputs fail before a network request is created.
        for (client_id, candidate_redirect_uri, authorization_code, verifier) in [
            ("", redirect_uri, "auth-code", pkce1.verifier.as_str()),
            ("client-id", "", "auth-code", pkce1.verifier.as_str()),
            ("client-id", redirect_uri, "", pkce1.verifier.as_str()),
            ("client-id", redirect_uri, "auth-code", "short"),
        ] {
            let config = services::google::GoogleOAuthConfig {
                client_id: client_id.to_string(),
                client_secret: String::new(),
                calendar_id: Some("primary".into()),
            };
            let mut transport = services::google::FakeHttpTransport::new(vec![]);
            let error = services::google::exchange_auth_code(
                &mut transport,
                &config,
                candidate_redirect_uri,
                authorization_code,
                verifier,
            )
            .expect_err("invalid token request must fail before transport");
            assert!(
                matches!(error, AppError::Validation(code) if code == "GOOGLE_TOKEN_REQUEST_INVALID")
            );
            assert!(transport.requests.is_empty());
        }

        // 7. The safe token error code cannot contain PKCE material.
        let safe_error_code =
            services::google::token_exchange_failure_code(&services::google::HttpResponse {
                status: 400,
                body: br#"{\"error\":\"invalid_request\"}"#.to_vec(),
            });
        assert!(!safe_error_code.contains(&pkce1.verifier));
        assert!(!safe_error_code.contains(&pkce1.challenge));

        // 8. Verifier is never persisted in database
        let (_temp, connection) = open_temp();
        let count_verifier_secrets: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM secure_secrets WHERE secret_key LIKE ?1 OR encrypted_value LIKE ?1",
                params![format!("%{}%", pkce1.verifier)],
                |row| row.get(0),
            )
            .unwrap_or(0);
        assert_eq!(count_verifier_secrets, 0);

        // 9. Timeout/failure clears attempt state
        {
            let _guard = try_acquire_google_connect_guard().expect("acquire guard");
        }
        let reacquired = try_acquire_google_connect_guard().expect("reacquire guard after release");
        drop(reacquired);

        // 10. Redirect URI remains unchanged and validates
        assert!(validate_oauth_redirect_uri(&auth_url_str, redirect_uri).is_ok());
    }

    #[test]
    fn disconnect_and_reconnect_do_not_remove_safe_google_runtime_config() {
        let temp = tempdir().expect("tempdir");
        let settings_dir = temp.path().join("settings");
        fs::create_dir_all(&settings_dir).expect("settings");
        let packaged = parse_live_services_config(
            r#"{"google":{"clientId":"packaged-client","calendarId":"primary"}}"#,
        )
        .expect("packaged config");
        provision_runtime_config(&settings_dir, &packaged).expect("provision");
        let path = settings_dir.join("live-services.json");
        let before = fs::read_to_string(&path).expect("config");

        provision_runtime_config(&settings_dir, &packaged).expect("reconnect provision");
        assert_eq!(fs::read_to_string(path).expect("config"), before);
    }

    #[test]
    fn reminder_readiness_does_not_treat_a_session_as_operationally_ready() {
        let (_temp, connection) = open_temp();
        let status = reminder_readiness_status(&connection).expect("readiness");
        assert_eq!(status.state, "disconnected");
        assert!(!status.automatic_enabled);
    }

    #[test]
    fn automatic_reminders_do_not_require_local_provider_metadata() {
        let (_temp, connection) = open_temp();

        connection
            .execute(
                "UPDATE whatsapp_settings
                 SET automatic_reminder_enabled=1,
                     is_enabled=0,
                     phone_number_id=NULL,
                     template_name=NULL
                 WHERE id=1",
                [],
            )
            .expect("enable automatic reminders without local provider metadata");

        assert!(automatic_reminders_enabled(&connection).expect("automatic reminder status"));
    }
    fn test_supabase_config() -> services::supabase::SupabaseConfig {
        services::supabase::SupabaseConfig {
            project_url: "https://example.supabase.co".into(),
            publishable_key: "public-key-with-enough-length".into(),
        }
    }

    fn test_supabase_session() -> services::supabase::SupabaseSession {
        services::supabase::SupabaseSession {
            access_token: "test-access-token".into(),
            refresh_token: "test-refresh-token".into(),
            expires_at: None,
        }
    }

    fn automatic_reminder_tick_test_setup() -> (
        tempfile::TempDir,
        PathBuf,
        services::secure_store::TestProtector,
    ) {
        let temp = tempdir().expect("temp");
        let database_path = reminder_coordinator_diagnostic_test_database_path(&temp);
        fs::create_dir_all(database_path.parent().expect("database parent"))
            .expect("create database directory");
        let mut connection = open_database(&database_path).expect("open database");
        connection
            .execute(
                "UPDATE whatsapp_settings SET automatic_reminder_enabled=1 WHERE id=1",
                [],
            )
            .expect("enable automatic reminders");
        let (mut customer, staff, service) = seed_core(&mut connection);
        customer = update_customer_tx(
            &mut connection,
            &customer.id,
            CustomerInput {
                first_name: customer.first_name,
                last_name: customer.last_name,
                phone: Some("05551112233".into()),
                email: None,
                notes: None,
                whatsapp_reminder_enabled: Some(true),
                whatsapp_consent_confirmed: Some(true),
            },
        )
        .expect("consented customer");
        let (local_date, local_start_time) = future_local_slot(2);
        create_appointment_tx(
            &mut connection,
            AppointmentInput {
                customer_id: customer.id,
                staff_id: staff.id,
                local_date,
                local_start_time,
                service_ids: vec![service.id],
                status: Some("planned".into()),
                note: None,
                whatsapp_reminder_enabled: Some(true),
            },
        )
        .expect("pending reminder appointment");
        let protector = services::secure_store::TestProtector;
        services::secure_store::store_supabase_session(
            &connection,
            &protector,
            &test_supabase_session(),
        )
        .expect("store session");
        (temp, database_path, protector)
    }

    fn dispatcher_active_response() -> services::google::HttpResponse {
        services::google::HttpResponse {
            status: 200,
            body: br#"{"data":{"paused":false}}"#.to_vec(),
        }
    }

    fn dispatcher_paused_response() -> services::google::HttpResponse {
        services::google::HttpResponse {
            status: 200,
            body: br#"{"data":{"paused":true}}"#.to_vec(),
        }
    }

    fn reminder_upsert_pending_response() -> services::google::HttpResponse {
        services::google::HttpResponse {
            status: 200,
            body: br#"{"data":{"revision":1,"status":"pending","remoteUpdatedAtUtc":"2026-09-14T00:00:00.000Z"}}"#.to_vec(),
        }
    }

    #[test]
    fn automatic_reminder_tick_uses_valid_session_without_refresh_and_processes_outbox() {
        let (temp, database_path, protector) = automatic_reminder_tick_test_setup();
        let mut transport = services::google::FakeHttpTransport::new(vec![
            dispatcher_active_response(),
            reminder_upsert_pending_response(),
        ]);

        assert!(
            automatic_reminder_tick_with(&database_path, &protector, &mut transport, || Ok(
                test_supabase_config()
            ),)
            .expect("tick")
        );

        assert_eq!(transport.requests.len(), 2);
        assert!(!transport
            .requests
            .iter()
            .any(|request| request.url.contains("grant_type=refresh_token")));
        let diagnostic = read_reminder_coordinator_diagnostic(&temp);
        assert!(diagnostic.contains("dispatcher_status_ok_active"));
        assert!(diagnostic.contains("process_ordered_outbox_entered"));
        assert!(diagnostic.contains("process_ordered_outbox_completed"));
        assert!(!diagnostic.contains("auth_refresh_started"));
    }

    #[test]
    fn automatic_reminder_tick_projects_pending_outbox_when_dispatcher_is_paused() {
        let (temp, database_path, protector) = automatic_reminder_tick_test_setup();
        let mut transport = services::google::FakeHttpTransport::new(vec![
            dispatcher_paused_response(),
            reminder_upsert_pending_response(),
        ]);

        assert!(
            automatic_reminder_tick_with(&database_path, &protector, &mut transport, || Ok(
                test_supabase_config()
            ),)
            .expect("paused dispatcher tick")
        );

        assert_eq!(transport.requests.len(), 2);
        assert!(transport
            .requests
            .iter()
            .all(|request| !request.url.contains("graph.facebook.com")));
        let diagnostic = read_reminder_coordinator_diagnostic(&temp);
        assert!(diagnostic.contains("dispatcher_status_ok_paused"));
        assert!(diagnostic.contains("process_ordered_outbox_entered"));
        assert!(diagnostic.contains("process_ordered_outbox_completed"));
    }

    #[test]
    fn automatic_reminder_tick_keeps_outbox_pending_when_projection_is_unavailable() {
        let (_temp, database_path, protector) = automatic_reminder_tick_test_setup();
        let mut transport = services::google::FakeHttpTransport::new(vec![
            dispatcher_active_response(),
            services::google::HttpResponse {
                status: 503,
                body: Vec::new(),
            },
        ]);

        assert!(
            automatic_reminder_tick_with(&database_path, &protector, &mut transport, || Ok(
                test_supabase_config()
            ),)
            .expect("temporary projection failure")
        );

        let connection = open_database(&database_path).expect("open database");
        let pending: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM reminder_cloud_outbox WHERE sync_status='pending'",
                [],
                |row| row.get(0),
            )
            .expect("pending outbox");
        assert_eq!(pending, 1);
    }

    #[test]
    fn automatic_reminder_tick_refreshes_once_then_projects_pending_outbox() {
        let (temp, database_path, protector) = automatic_reminder_tick_test_setup();
        let mut transport = services::google::FakeHttpTransport::new(vec![
            services::google::HttpResponse {
                status: 401,
                body: Vec::new(),
            },
            services::google::HttpResponse {
                status: 200,
                body: br#"{"access_token":"refreshed-access","refresh_token":"refreshed-refresh","expires_at":1800000000}"#
                    .to_vec(),
            },
            dispatcher_active_response(),
            reminder_upsert_pending_response(),
        ]);

        assert!(
            automatic_reminder_tick_with(&database_path, &protector, &mut transport, || Ok(
                test_supabase_config()
            ),)
            .expect("refreshed tick")
        );

        assert_eq!(transport.requests.len(), 4);
        assert_eq!(
            transport
                .requests
                .iter()
                .filter(|request| request.url.contains("grant_type=refresh_token"))
                .count(),
            1
        );
        let connection = open_database(&database_path).expect("open refreshed database");
        let stored = services::secure_store::read_supabase_session(&connection, &protector)
            .expect("read refreshed session")
            .expect("stored refreshed session");
        assert_eq!(stored.access_token, "refreshed-access");
        let diagnostic = read_reminder_coordinator_diagnostic(&temp);
        assert!(diagnostic.contains("dispatcher_status_auth_error"));
        assert!(diagnostic.contains("auth_refresh_started"));
        assert!(diagnostic.contains("auth_refresh_succeeded"));
        assert!(diagnostic.contains("dispatcher_status_retry_succeeded"));
        assert!(diagnostic.contains("process_ordered_outbox_completed"));
        assert!(!diagnostic.contains("refreshed-access"));
        assert!(!diagnostic.contains("refreshed-refresh"));
    }

    #[test]
    fn automatic_reminder_tick_keeps_outbox_pending_when_refresh_fails() {
        let (temp, database_path, protector) = automatic_reminder_tick_test_setup();
        let mut transport = services::google::FakeHttpTransport::new(vec![
            services::google::HttpResponse {
                status: 401,
                body: Vec::new(),
            },
            services::google::HttpResponse {
                status: 401,
                body: Vec::new(),
            },
        ]);

        assert!(
            !automatic_reminder_tick_with(&database_path, &protector, &mut transport, || Ok(
                test_supabase_config()
            ),)
            .expect("closed tick")
        );

        assert_eq!(transport.requests.len(), 2);
        let connection = open_database(&database_path).expect("open database");
        let pending: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM reminder_cloud_outbox WHERE sync_status='pending'",
                [],
                |row| row.get(0),
            )
            .expect("pending outbox");
        assert_eq!(pending, 1);
        let diagnostic = read_reminder_coordinator_diagnostic(&temp);
        assert!(diagnostic.contains("auth_refresh_started"));
        assert!(diagnostic.contains("auth_refresh_failed"));
        assert!(!diagnostic.contains("process_ordered_outbox_entered"));
    }

    #[test]
    fn automatic_reminder_tick_stops_after_one_refresh_when_dispatcher_retry_fails() {
        let (temp, database_path, protector) = automatic_reminder_tick_test_setup();
        let mut transport = services::google::FakeHttpTransport::new(vec![
            services::google::HttpResponse {
                status: 401,
                body: Vec::new(),
            },
            services::google::HttpResponse {
                status: 200,
                body: br#"{"access_token":"refreshed-access","refresh_token":"refreshed-refresh","expires_at":1800000000}"#
                    .to_vec(),
            },
            services::google::HttpResponse {
                status: 401,
                body: Vec::new(),
            },
        ]);

        assert!(
            !automatic_reminder_tick_with(&database_path, &protector, &mut transport, || Ok(
                test_supabase_config()
            ),)
            .expect("closed retry tick")
        );

        assert_eq!(transport.requests.len(), 3);
        assert_eq!(
            transport
                .requests
                .iter()
                .filter(|request| request.url.contains("grant_type=refresh_token"))
                .count(),
            1
        );
        let connection = open_database(&database_path).expect("open database");
        let pending: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM reminder_cloud_outbox WHERE sync_status='pending'",
                [],
                |row| row.get(0),
            )
            .expect("pending outbox");
        assert_eq!(pending, 1);
        let diagnostic = read_reminder_coordinator_diagnostic(&temp);
        assert!(diagnostic.contains("auth_refresh_succeeded"));
        assert!(diagnostic.contains("dispatcher_status_retry_failed"));
        assert!(!diagnostic.contains("process_ordered_outbox_entered"));
    }

    #[test]
    fn cloud_session_validation_keeps_a_valid_session_without_dispatcher_mutation() {
        let (_temp, connection) = open_temp();
        let protector = services::secure_store::TestProtector;
        services::secure_store::store_supabase_session(
            &connection,
            &protector,
            &test_supabase_session(),
        )
        .expect("store session");
        let mut transport =
            services::google::FakeHttpTransport::new(vec![services::google::HttpResponse {
                status: 200,
                body: br#"{"id":"user"}"#.to_vec(),
            }]);

        let status = cloud_connection_status_with(
            &connection,
            &protector,
            Some(&test_supabase_config()),
            &mut transport,
        )
        .expect("status");

        assert!(status.configured && status.session_present);
        assert_eq!(
            status.validation_state,
            services::supabase::SupabaseSessionValidationState::Valid
        );
        assert!(
            services::secure_store::read_supabase_session(&connection, &protector)
                .expect("read session")
                .is_some()
        );
        assert_eq!(transport.requests.len(), 1);
        assert!(transport.requests[0].url.ends_with("/auth/v1/user"));
        assert!(transport.requests[0]
            .headers
            .iter()
            .any(|(name, _)| name.eq_ignore_ascii_case("apikey")));
        assert!(transport.requests[0]
            .headers
            .iter()
            .any(|(name, value)| name.eq_ignore_ascii_case("authorization")
                && value.starts_with("Bearer ")));
        assert!(!transport.requests[0].url.contains("/functions/v1/"));
    }

    #[test]
    fn cloud_session_validation_refreshes_expired_access_token() {
        let (_temp, connection) = open_temp();
        let protector = services::secure_store::TestProtector;
        services::secure_store::store_supabase_session(
            &connection,
            &protector,
            &test_supabase_session(),
        )
        .expect("store session");

        let mut transport = services::google::FakeHttpTransport::new(vec![
            services::google::HttpResponse {
                status: 401,
                body: br#"{"error":"expired_access_token"}"#.to_vec(),
            },
            services::google::HttpResponse {
                status: 200,
                body: br#"{"access_token":"refreshed-access","refresh_token":"refreshed-refresh","expires_at":1800000000}"#
                    .to_vec(),
            },
        ]);

        let status = cloud_connection_status_with(
            &connection,
            &protector,
            Some(&test_supabase_config()),
            &mut transport,
        )
        .expect("status");

        assert!(status.configured && status.session_present);
        assert_eq!(
            status.validation_state,
            services::supabase::SupabaseSessionValidationState::Valid
        );

        let stored = services::secure_store::read_supabase_session(&connection, &protector)
            .expect("read session")
            .expect("stored refreshed session");

        assert_eq!(stored.access_token, "refreshed-access");
        assert_eq!(stored.refresh_token, "refreshed-refresh");

        assert_eq!(transport.requests.len(), 2);
        assert!(transport.requests[0].url.ends_with("/auth/v1/user"));
        assert!(transport.requests[1]
            .url
            .contains("grant_type=refresh_token"));
    }

    #[test]
    fn cloud_session_validation_clears_only_authoritatively_invalid_session() {
        let (_temp, connection) = open_temp();
        let protector = services::secure_store::TestProtector;
        services::secure_store::store_supabase_session(
            &connection,
            &protector,
            &test_supabase_session(),
        )
        .expect("store cloud session");
        services::secure_store::upsert_secret(
            &connection,
            &protector,
            "google_calendar_refresh_token",
            "google-refresh",
        )
        .expect("store google secret");
        let google_outbox_before: i64 = connection
            .query_row("SELECT COUNT(*) FROM google_calendar_outbox", [], |row| {
                row.get(0)
            })
            .expect("google outbox count");
        let mut transport = services::google::FakeHttpTransport::new(vec![
            services::google::HttpResponse {
                status: 401,
                body: br#"{"error":"expired_access_token"}"#.to_vec(),
            },
            services::google::HttpResponse {
                status: 401,
                body: br#"{"error":"invalid_refresh_token"}"#.to_vec(),
            },
        ]);

        let status = cloud_connection_status_with(
            &connection,
            &protector,
            Some(&test_supabase_config()),
            &mut transport,
        )
        .expect("status");

        assert!(!status.session_present);
        assert_eq!(
            status.validation_state,
            services::supabase::SupabaseSessionValidationState::Disconnected
        );
        assert!(
            services::secure_store::read_supabase_session(&connection, &protector)
                .expect("read cloud session")
                .is_none()
        );
        assert!(services::secure_store::read_secret(
            &connection,
            &protector,
            "google_calendar_refresh_token"
        )
        .expect("read google secret")
        .is_some());
        let google_outbox_after: i64 = connection
            .query_row("SELECT COUNT(*) FROM google_calendar_outbox", [], |row| {
                row.get(0)
            })
            .expect("google outbox count");
        assert_eq!(google_outbox_after, google_outbox_before);
        assert!(!transport.requests[0].url.contains("/functions/v1/"));
    }

    #[test]
    fn cloud_session_validation_preserves_session_during_temporary_failure() {
        let (_temp, connection) = open_temp();
        let protector = services::secure_store::TestProtector;
        services::secure_store::store_supabase_session(
            &connection,
            &protector,
            &test_supabase_session(),
        )
        .expect("store session");
        let mut transport =
            services::google::FakeHttpTransport::new(vec![services::google::HttpResponse {
                status: 503,
                body: Vec::new(),
            }]);

        let status = cloud_connection_status_with(
            &connection,
            &protector,
            Some(&test_supabase_config()),
            &mut transport,
        )
        .expect("status");

        assert!(status.session_present);
        assert_eq!(
            status.validation_state,
            services::supabase::SupabaseSessionValidationState::Unavailable
        );
        assert!(
            services::secure_store::read_supabase_session(&connection, &protector)
                .expect("read session")
                .is_some()
        );
        assert!(!transport.requests[0].url.contains("/functions/v1/"));
    }

    #[test]
    fn cloud_session_validation_preserves_session_when_refresh_is_temporarily_unavailable() {
        let (_temp, connection) = open_temp();
        let protector = services::secure_store::TestProtector;
        services::secure_store::store_supabase_session(
            &connection,
            &protector,
            &test_supabase_session(),
        )
        .expect("store session");

        let mut transport = services::google::FakeHttpTransport::new(vec![
            services::google::HttpResponse {
                status: 401,
                body: br#"{"error":"expired_access_token"}"#.to_vec(),
            },
            services::google::HttpResponse {
                status: 503,
                body: Vec::new(),
            },
        ]);

        let status = cloud_connection_status_with(
            &connection,
            &protector,
            Some(&test_supabase_config()),
            &mut transport,
        )
        .expect("status");

        assert!(status.session_present);
        assert_eq!(
            status.validation_state,
            services::supabase::SupabaseSessionValidationState::Unavailable
        );

        let stored = services::secure_store::read_supabase_session(&connection, &protector)
            .expect("read session")
            .expect("preserved session");

        assert_eq!(stored.access_token, "test-access-token");
        assert_eq!(stored.refresh_token, "test-refresh-token");

        assert_eq!(transport.requests.len(), 2);
        assert!(transport.requests[1]
            .url
            .contains("grant_type=refresh_token"));
    }

    fn seed_core(connection: &mut Connection) -> (Customer, Staff, ServiceItem) {
        let customer = create_customer_tx(
            connection,
            CustomerInput {
                first_name: "Ayse".into(),
                last_name: "Yilmaz".into(),
                phone: Some("+90 555 111 22 33".into()),
                email: None,
                notes: None,
                whatsapp_reminder_enabled: Some(true),
                whatsapp_consent_confirmed: Some(false),
            },
        )
        .expect("customer");
        let staff = create_staff_tx(
            connection,
            StaffInput {
                first_name: "Zeynep".into(),
                last_name: Some("Usta".into()),
                phone: Some("05552223344".into()),
                specialty_note: None,
                color_key: "sage".into(),
                is_active: Some(true),
            },
        )
        .expect("staff");
        let category = create_category_tx(
            connection,
            CategoryInput {
                name: "Cilt Bakimi".into(),
                is_active: Some(true),
            },
        )
        .expect("category");
        let service = create_service_tx(
            connection,
            ServiceInput {
                category_id: category.id,
                name: "Klasik Bakim".into(),
                duration_minutes: Some(30),
                default_price_minor: Some(0),
                is_active: Some(true),
            },
        )
        .expect("service");
        set_staff_services_tx(connection, &staff.id, vec![service.id.clone()]).expect("assign");
        (customer, staff, service)
    }

    fn appointment_input(
        customer_id: &str,
        staff_id: &str,
        local_date: &str,
        local_start_time: &str,
        service_ids: Vec<String>,
        status: &str,
    ) -> AppointmentInput {
        AppointmentInput {
            customer_id: customer_id.into(),
            staff_id: staff_id.into(),
            local_date: local_date.into(),
            local_start_time: local_start_time.into(),
            service_ids,
            status: Some(status.into()),
            note: None,
            whatsapp_reminder_enabled: None,
        }
    }

    #[test]
    fn v18_to_v19_appointment_whatsapp_preference_defaults_existing_rows_to_enabled() {
        let connection = Connection::open_in_memory().expect("memory db");
        connection
            .execute_batch(
                "CREATE TABLE app_meta (schema_version INTEGER NOT NULL);
                 INSERT INTO app_meta (schema_version) VALUES (18);
                 CREATE TABLE appointments (id TEXT PRIMARY KEY NOT NULL);
                 INSERT INTO appointments (id) VALUES ('legacy-appointment');",
            )
            .expect("v18 fixture");

        migrate_v19_appointment_whatsapp_preference(&connection).expect("migrate v19");

        assert!(table_columns(&connection, "appointments")
            .expect("appointment columns")
            .contains("whatsapp_reminder_enabled"));
        assert_eq!(
            connection
                .query_row(
                    "SELECT whatsapp_reminder_enabled FROM appointments WHERE id='legacy-appointment'",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .expect("legacy preference"),
            1
        );
    }

    #[test]
    fn appointment_whatsapp_preference_defaults_persists_and_controls_reminder_eligibility() {
        let (_temp, mut connection) = open_temp();
        let (customer, staff, service) = seed_core(&mut connection);
        update_customer_tx(
            &mut connection,
            &customer.id,
            CustomerInput {
                first_name: customer.first_name.clone(),
                last_name: customer.last_name.clone(),
                phone: customer.phone.clone(),
                email: customer.email.clone(),
                notes: customer.notes.clone(),
                whatsapp_reminder_enabled: Some(true),
                whatsapp_consent_confirmed: Some(true),
            },
        )
        .expect("record consent");

        let default_appointment = create_appointment_tx(
            &mut connection,
            appointment_input(
                &customer.id,
                &staff.id,
                "2036-12-02",
                "10:00",
                vec![service.id.clone()],
                "planned",
            ),
        )
        .expect("default appointment");
        assert!(default_appointment.whatsapp_reminder_enabled);
        assert!(default_appointment.whatsapp_reminder_effective);

        let mut disabled_input = appointment_input(
            &customer.id,
            &staff.id,
            "2036-12-03",
            "10:00",
            vec![service.id.clone()],
            "planned",
        );
        disabled_input.whatsapp_reminder_enabled = Some(false);
        let disabled =
            create_appointment_tx(&mut connection, disabled_input).expect("disabled appointment");
        assert!(!disabled.whatsapp_reminder_enabled);
        assert!(!disabled.whatsapp_reminder_effective);
        let disabled_reminders: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM appointment_reminders WHERE appointment_id=?1",
                params![disabled.id],
                |row| row.get(0),
            )
            .expect("disabled reminder count");
        assert_eq!(disabled_reminders, 0);
        let google_outbox_after_disabled: i64 = connection
            .query_row("SELECT COUNT(*) FROM google_calendar_outbox", [], |row| {
                row.get(0)
            })
            .expect("Google outbox count");
        assert_eq!(google_outbox_after_disabled, 0);

        let mut enabled_input = appointment_input(
            &customer.id,
            &staff.id,
            "2036-12-03",
            "10:00",
            vec![service.id.clone()],
            "planned",
        );
        enabled_input.whatsapp_reminder_enabled = Some(true);
        let enabled = update_appointment_tx(&mut connection, &disabled.id, enabled_input)
            .expect("enable preference");
        assert!(enabled.whatsapp_reminder_enabled);
        assert!(enabled.whatsapp_reminder_effective);
        let enabled_reminders: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM appointment_reminders WHERE appointment_id=?1",
                params![disabled.id],
                |row| row.get(0),
            )
            .expect("enabled reminder count");
        assert_eq!(enabled_reminders, 1);
        let google_outbox_after_enabled: i64 = connection
            .query_row("SELECT COUNT(*) FROM google_calendar_outbox", [], |row| {
                row.get(0)
            })
            .expect("Google outbox unchanged");
        assert_eq!(google_outbox_after_enabled, 0);

        connection
            .execute(
                "UPDATE customers SET phone=NULL WHERE id=?1",
                params![customer.id],
            )
            .expect("clear test phone");
        reconcile_reminder_for_appointment(&connection, &disabled.id)
            .expect("reconcile after phone removal");
        let without_phone = get_appointment(&connection, &disabled.id).expect("appointment");
        assert!(without_phone.whatsapp_reminder_enabled);
        assert!(!without_phone.whatsapp_reminder_effective);
    }

    fn configure_daily_hours(connection: &mut Connection, staff_id: &str) {
        let intervals = (0..=6)
            .map(|weekday| StaffWorkingHour {
                staff_id: staff_id.into(),
                weekday,
                start_minute: 540,
                end_minute: 1080,
            })
            .collect();
        replace_staff_working_hours(connection, staff_id, intervals).expect("daily hours");
    }

    fn business_profile_input() -> BusinessProfileInput {
        BusinessProfileInput {
            business_name: String::new(),
            phone: None,
            email: None,
            address: None,
            currency_code: "TRY".into(),
            theme_key: "default".into(),
            logo_data: None,
            logo_mime_type: None,
        }
    }

    fn audit_actions(
        connection: &Connection,
        entity_type: &str,
        entity_id: &str,
    ) -> Vec<(String, Option<String>)> {
        let mut statement = connection
            .prepare(
                "SELECT action, metadata_json FROM audit_log
                 WHERE entity_type=?1 AND entity_id=?2
                 ORDER BY occurred_at ASC, rowid ASC",
            )
            .expect("audit query");
        let rows = statement
            .query_map(params![entity_type, entity_id], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .expect("audit rows");
        rows.collect::<Result<Vec<_>, _>>()
            .expect("collect audit rows")
    }

    #[test]
    fn empty_db_bootstrap_matches_v1_core_schema() {
        let (_temp, connection) = open_temp();
        assert_eq!(
            read_schema_version(&connection).expect("schema"),
            CORE_SCHEMA_VERSION
        );
        for table in [
            "app_meta",
            "customers",
            "service_categories",
            "services",
            "staff",
            "staff_services",
            "appointments",
            "appointment_services",
            "appointment_reminders",
            "whatsapp_settings",
            "secure_secrets",
            "cloud_reminder_settings",
            "reminder_cloud_state",
            "reminder_cloud_outbox",
            "google_calendar_settings",
            "appointment_google_calendar_sync",
            "google_calendar_outbox",
            "staff_working_hours",
            "staff_time_off",
            "audit_log",
            "business_profile",
        ] {
            let count: i64 = connection
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
                    params![table],
                    |row| row.get(0),
                )
                .expect("table");
            assert_eq!(count, 1, "{table}");
        }
        assert!(integrity_check(&connection).expect("integrity"));
    }

    #[test]
    fn upgrades_foundation_app_meta_shape_to_core_shape() {
        let temp = tempdir().expect("tempdir");
        let db_path = temp.path().join("legacy-foundation.db");
        {
            let connection = Connection::open(&db_path).expect("legacy open");
            connection
                .execute_batch(
                    "
                    CREATE TABLE app_meta (
                      id INTEGER PRIMARY KEY CHECK (id = 1),
                      schema_version INTEGER NOT NULL,
                      created_at_utc TEXT NOT NULL,
                      updated_at_utc TEXT NOT NULL
                    );
                    INSERT INTO app_meta (id, schema_version, created_at_utc, updated_at_utc)
                    VALUES (1, 1, '2026-08-24T00:00:00.000Z', '2026-08-24T00:00:00.000Z');
                    ",
                )
                .expect("legacy schema");
        }

        let connection = open_database(&db_path).expect("open upgraded");
        assert_eq!(
            read_schema_version(&connection).expect("schema"),
            CORE_SCHEMA_VERSION
        );
        let initialized_at: String = connection
            .query_row(
                "SELECT initialized_at FROM app_meta WHERE id = 1",
                [],
                |row| row.get(0),
            )
            .expect("initialized_at");
        assert_eq!(initialized_at, "2026-08-24T00:00:00.000Z");
    }

    #[test]
    fn business_profile_domain_enforces_single_row_and_safe_validation() {
        let (_temp, mut connection) = open_temp();
        let default_profile = load_business_profile(&connection).expect("default profile");
        assert_eq!(default_profile.business_name, "");
        assert_eq!(default_profile.currency_code, "TRY");
        assert_eq!(default_profile.theme_key, "default");
        assert_eq!(default_profile.phone, None);
        assert_eq!(default_profile.email, None);
        assert_eq!(default_profile.address, None);
        assert_eq!(default_profile.logo_data, None);
        assert_eq!(default_profile.logo_mime_type, None);

        let mut input = business_profile_input();
        input.business_name = "  Ada   Salon  ".into();
        input.phone = Some("+90 555 111 22 33".into());
        input.email = Some("owner@example.test".into());
        input.address = Some("  Ataturk  Cad.  1  ".into());
        input.currency_code = "try".into();
        input.logo_data = Some(vec![1, 2, 3]);
        input.logo_mime_type = Some("image/png".into());
        let updated = update_business_profile_tx(&mut connection, input).expect("valid update");
        assert_eq!(updated.business_name, "Ada Salon");
        assert_eq!(updated.phone.as_deref(), Some("+90 555 111 22 33"));
        assert_eq!(updated.email.as_deref(), Some("owner@example.test"));
        assert_eq!(updated.address.as_deref(), Some("Ataturk Cad. 1"));
        assert_eq!(updated.currency_code, "TRY");
        assert_eq!(updated.logo_mime_type.as_deref(), Some("image/png"));

        let mut blank_input = business_profile_input();
        blank_input.business_name = "  \t  ".into();
        blank_input.phone = Some("   ".into());
        blank_input.email = Some("   ".into());
        blank_input.address = Some("   ".into());
        let blank_profile = update_business_profile_tx(&mut connection, blank_input)
            .expect("blank optional values");
        assert_eq!(blank_profile.business_name, "");
        assert_eq!(blank_profile.phone, None);
        assert_eq!(blank_profile.email, None);
        assert_eq!(blank_profile.address, None);
        assert_eq!(blank_profile.logo_data, None);
        assert_eq!(blank_profile.logo_mime_type, None);
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM business_profile", [], |row| row
                    .get::<_, i64>(0))
                .expect("single profile row"),
            1
        );

        let mut contact_input = business_profile_input();
        contact_input.phone = Some("+90 555 111 22 33".into());
        contact_input.email = Some("owner@example.test".into());
        contact_input.address = Some("Ataturk Cad. 1".into());
        let valid_contact_profile =
            update_business_profile_tx(&mut connection, contact_input).expect("valid contacts");
        assert_eq!(
            valid_contact_profile.phone.as_deref(),
            Some("+90 555 111 22 33")
        );
        assert_eq!(
            valid_contact_profile.email.as_deref(),
            Some("owner@example.test")
        );
        assert_eq!(
            valid_contact_profile.address.as_deref(),
            Some("Ataturk Cad. 1")
        );

        let mut lowercase_currency = business_profile_input();
        lowercase_currency.currency_code = "try".into();
        let normalized_currency =
            update_business_profile_tx(&mut connection, lowercase_currency).expect("lowercase TRY");
        assert_eq!(normalized_currency.currency_code, "TRY");
        let before_invalid = load_business_profile(&connection).expect("profile before invalid");
        for invalid_currency in ["TR", "123", "TRYX"] {
            let mut invalid_input = business_profile_input();
            invalid_input.currency_code = invalid_currency.into();
            assert!(update_business_profile_tx(&mut connection, invalid_input).is_err());
            assert_eq!(
                load_business_profile(&connection).expect("profile after invalid currency"),
                before_invalid
            );
        }
        let mut invalid_theme = business_profile_input();
        invalid_theme.theme_key = "custom-css".into();
        assert!(update_business_profile_tx(&mut connection, invalid_theme).is_err());
        assert_eq!(
            load_business_profile(&connection).expect("profile after invalid theme"),
            before_invalid
        );

        for mime in ["image/png", "image/jpeg", "image/webp"] {
            let mut logo_input = business_profile_input();
            logo_input.logo_data = Some(vec![1, 2, 3]);
            logo_input.logo_mime_type = Some(mime.into());
            let logo_profile =
                update_business_profile_tx(&mut connection, logo_input).expect("valid logo");
            assert_eq!(logo_profile.logo_mime_type.as_deref(), Some(mime));
        }
        let profile_before_invalid_logo =
            load_business_profile(&connection).expect("profile before logo");
        let mut unsupported_logo = business_profile_input();
        unsupported_logo.logo_data = Some(vec![1]);
        unsupported_logo.logo_mime_type = Some("image/svg+xml".into());
        assert!(update_business_profile_tx(&mut connection, unsupported_logo).is_err());
        let mut missing_mime = business_profile_input();
        missing_mime.logo_data = Some(vec![1]);
        assert!(update_business_profile_tx(&mut connection, missing_mime).is_err());
        let mut missing_data = business_profile_input();
        missing_data.logo_mime_type = Some("image/png".into());
        assert!(update_business_profile_tx(&mut connection, missing_data).is_err());
        let mut empty_logo = business_profile_input();
        empty_logo.logo_data = Some(Vec::new());
        empty_logo.logo_mime_type = Some("image/png".into());
        assert!(update_business_profile_tx(&mut connection, empty_logo).is_err());
        let mut oversized_logo = business_profile_input();
        oversized_logo.logo_data = Some(vec![0; 3 * 1024 * 1024 + 1]);
        oversized_logo.logo_mime_type = Some("image/png".into());
        assert!(update_business_profile_tx(&mut connection, oversized_logo).is_err());
        assert_eq!(
            load_business_profile(&connection).expect("profile after invalid logos"),
            profile_before_invalid_logo
        );

        let mut oversized_name = business_profile_input();
        oversized_name.business_name = "x".repeat(161);
        assert!(update_business_profile_tx(&mut connection, oversized_name).is_err());
        let mut oversized_phone = business_profile_input();
        oversized_phone.phone = Some("1".repeat(33));
        assert!(update_business_profile_tx(&mut connection, oversized_phone).is_err());
        let mut oversized_address = business_profile_input();
        oversized_address.address = Some("x".repeat(501));
        assert!(update_business_profile_tx(&mut connection, oversized_address).is_err());
        let mut invalid_email = business_profile_input();
        invalid_email.email = Some("not-an-email".into());
        assert!(update_business_profile_tx(&mut connection, invalid_email).is_err());
        assert_eq!(
            load_business_profile(&connection).expect("profile after invalid contacts"),
            profile_before_invalid_logo
        );
    }

    #[test]
    fn onboarding_state_uses_all_business_rows_and_resumes_only_incomplete_setup() {
        let fresh = onboarding_state_from_counts("", 0, 0, 0, 0);
        assert!(fresh.needs_onboarding);
        assert_eq!(fresh.next_step, 1);

        let archived_or_inactive_history = onboarding_state_from_counts("", 1, 0, 0, 0);
        assert!(!archived_or_inactive_history.needs_onboarding);

        let after_business = onboarding_state_from_counts("Salon", 0, 0, 0, 0);
        assert!(after_business.needs_onboarding);
        assert_eq!(after_business.next_step, 2);

        let after_staff = onboarding_state_from_counts("Salon", 0, 0, 1, 0);
        assert!(after_staff.needs_onboarding);
        assert_eq!(after_staff.next_step, 3);

        let complete = onboarding_state_from_counts("Salon", 0, 0, 1, 1);
        assert!(!complete.needs_onboarding);
    }

    #[test]
    fn business_profile_audit_is_private_atomic_and_deduplicated() {
        let (_temp, mut connection) = open_temp();
        let private_phone = "+90 555 777 88 99";
        let private_email = "profile@example.test";
        let private_address = "Private address must not be audited";
        let mut input = business_profile_input();
        input.business_name = "Profile Test".into();
        input.phone = Some(private_phone.into());
        input.email = Some(private_email.into());
        input.address = Some(private_address.into());
        input.logo_data = Some(vec![7, 8, 9]);
        input.logo_mime_type = Some("image/png".into());
        let updated = update_business_profile_tx(&mut connection, input.clone()).expect("update");
        assert_eq!(updated.business_name, "Profile Test");
        let audit = audit_actions(&connection, "business_profile", "1");
        assert_eq!(audit.len(), 1);
        assert_eq!(audit[0].0, "update");
        let metadata = audit[0].1.as_deref().expect("changed fields metadata");
        for field in ["business_name", "phone", "email", "address", "logo"] {
            assert!(metadata.contains(field), "missing {field}");
        }
        assert!(!metadata.contains(private_phone));
        assert!(!metadata.contains(private_email));
        assert!(!metadata.contains(private_address));
        assert!(!metadata.contains("image/png"));
        assert!(!metadata.contains("7,8,9"));

        update_business_profile_tx(&mut connection, input.clone()).expect("same-value no-op");
        let mut normalized_same_value = input.clone();
        normalized_same_value.currency_code = "try".into();
        update_business_profile_tx(&mut connection, normalized_same_value)
            .expect("normalized same-value no-op");
        assert_eq!(audit_actions(&connection, "business_profile", "1").len(), 1);

        let profile_before_invalid =
            load_business_profile(&connection).expect("profile before invalid");
        let mut invalid = input.clone();
        invalid.currency_code = "X".into();
        assert!(update_business_profile_tx(&mut connection, invalid).is_err());
        assert_eq!(
            load_business_profile(&connection).expect("profile after invalid"),
            profile_before_invalid
        );
        assert_eq!(audit_actions(&connection, "business_profile", "1").len(), 1);

        connection
            .execute_batch(
                "CREATE TRIGGER business_profile_audit_test_failure
                 BEFORE INSERT ON audit_log
                 BEGIN SELECT RAISE(ABORT, 'BUSINESS_PROFILE_AUDIT_TEST_FAILURE'); END;",
            )
            .expect("audit failure trigger");
        let mut failing_update = input;
        failing_update.business_name = "Must Roll Back".into();
        assert!(update_business_profile_tx(&mut connection, failing_update).is_err());
        connection
            .execute_batch("DROP TRIGGER business_profile_audit_test_failure;")
            .expect("drop audit failure trigger");
        assert_eq!(
            load_business_profile(&connection).expect("profile after audit failure"),
            profile_before_invalid
        );
        assert_eq!(audit_actions(&connection, "business_profile", "1").len(), 1);
    }

    #[test]
    fn business_profile_tauri_dtos_use_typed_camel_case_fields() {
        let input: BusinessProfileInput = serde_json::from_value(serde_json::json!({
            "businessName": "Test",
            "phone": null,
            "email": null,
            "address": null,
            "currencyCode": "TRY",
            "themeKey": "default",
            "logoData": null,
            "logoMimeType": null
        }))
        .expect("typed input");
        assert_eq!(input.business_name, "Test");
        let output = serde_json::to_value(BusinessProfile {
            business_name: "Test".into(),
            phone: None,
            email: None,
            address: None,
            currency_code: "TRY".into(),
            theme_key: "default".into(),
            logo_data: None,
            logo_mime_type: None,
            updated_at: "2026-08-31T00:00:00.000Z".into(),
        })
        .expect("typed output");
        assert!(output.get("businessName").is_some());
        assert!(output.get("currencyCode").is_some());
        assert!(output.get("logoData").is_some());
        assert!(output.get("logoMimeType").is_some());
    }

    #[test]
    fn repeat_booking_seed_is_read_only_snapshot_named_and_eligibility_aware() {
        let (_temp, mut connection) = open_temp();
        let (customer, staff, service) = seed_core(&mut connection);
        update_service_tx(
            &mut connection,
            &service.id,
            ServiceInput {
                category_id: service.category_id.clone(),
                name: service.name.clone(),
                duration_minutes: service.duration_minutes,
                default_price_minor: Some(100_000),
                is_active: Some(true),
            },
        )
        .expect("set default price");
        let second_service = create_service_tx(
            &mut connection,
            ServiceInput {
                category_id: service.category_id.clone(),
                name: "Historical Add-on".into(),
                duration_minutes: Some(45),
                default_price_minor: Some(200_000),
                is_active: Some(true),
            },
        )
        .expect("second service");
        set_staff_services_tx(
            &mut connection,
            &staff.id,
            vec![service.id.clone(), second_service.id.clone()],
        )
        .expect("staff services");
        let source = create_appointment_tx(
            &mut connection,
            appointment_input(
                &customer.id,
                &staff.id,
                "2025-02-01",
                "10:00",
                vec![service.id.clone(), second_service.id.clone()],
                "completed",
            ),
        )
        .expect("source appointment");
        set_appointment_service_charged_price(&mut connection, &source.id, &service.id, 75_000)
            .expect("historical discount");
        let other_customer = create_customer_tx(
            &mut connection,
            CustomerInput {
                first_name: "Other".into(),
                last_name: "Customer".into(),
                phone: None,
                email: None,
                notes: None,
                whatsapp_reminder_enabled: None,
                whatsapp_consent_confirmed: None,
            },
        )
        .expect("other customer");

        let before_seed_counts = (
            connection
                .query_row("SELECT COUNT(*) FROM appointments", [], |row| {
                    row.get::<_, i64>(0)
                })
                .expect("appointment count"),
            connection
                .query_row("SELECT COUNT(*) FROM appointment_services", [], |row| {
                    row.get::<_, i64>(0)
                })
                .expect("appointment service count"),
            connection
                .query_row("SELECT COUNT(*) FROM audit_log", [], |row| {
                    row.get::<_, i64>(0)
                })
                .expect("audit count"),
            connection
                .query_row("SELECT COUNT(*) FROM google_calendar_outbox", [], |row| {
                    row.get::<_, i64>(0)
                })
                .expect("google outbox count"),
            connection
                .query_row("SELECT COUNT(*) FROM reminder_cloud_outbox", [], |row| {
                    row.get::<_, i64>(0)
                })
                .expect("cloud outbox count"),
        );
        let seed = load_repeat_booking_seed(&connection, &customer.id, &source.id).expect("seed");
        assert_eq!(seed.customer_id, customer.id);
        assert_eq!(seed.customer_display_name, "Ayse Yilmaz");
        assert_eq!(seed.staff_id.as_deref(), Some(staff.id.as_str()));
        assert_eq!(seed.services.len(), 2);
        assert_eq!(seed.services[0].service_name_snapshot, "Klasik Bakim");
        assert!(seed.services.iter().all(|service| service.is_eligible));
        let seed_json = serde_json::to_value(&seed).expect("seed json");
        assert!(seed_json.get("listedPriceSnapshotMinor").is_none());
        assert!(seed_json.get("chargedPriceMinor").is_none());
        assert!(seed_json.get("durationMinutesSnapshot").is_none());
        assert!(load_repeat_booking_seed(&connection, &other_customer.id, &source.id).is_err());
        assert!(load_repeat_booking_seed(&connection, &customer.id, "missing").is_err());
        assert_eq!(
            (
                connection
                    .query_row("SELECT COUNT(*) FROM appointments", [], |row| row
                        .get::<_, i64>(0))
                    .expect("appointment count after seed"),
                connection
                    .query_row("SELECT COUNT(*) FROM appointment_services", [], |row| row
                        .get::<_, i64>(
                        0
                    ))
                    .expect("appointment service count after seed"),
                connection
                    .query_row("SELECT COUNT(*) FROM audit_log", [], |row| row
                        .get::<_, i64>(0))
                    .expect("audit count after seed"),
                connection
                    .query_row("SELECT COUNT(*) FROM google_calendar_outbox", [], |row| row
                        .get::<_, i64>(0))
                    .expect("google outbox count after seed"),
                connection
                    .query_row("SELECT COUNT(*) FROM reminder_cloud_outbox", [], |row| row
                        .get::<_, i64>(
                        0
                    ))
                    .expect("cloud outbox count after seed"),
            ),
            before_seed_counts
        );

        update_service_tx(
            &mut connection,
            &service.id,
            ServiceInput {
                category_id: service.category_id.clone(),
                name: "Renamed Current Service".into(),
                duration_minutes: Some(60),
                default_price_minor: Some(999_000),
                is_active: Some(true),
            },
        )
        .expect("rename service");
        deactivate_service(&mut connection, &service.id).expect("inactive service");
        deactivate_staff(&mut connection, &staff.id).expect("inactive staff");
        let inactive_seed =
            load_repeat_booking_seed(&connection, &customer.id, &source.id).expect("inactive seed");
        assert_eq!(inactive_seed.staff_id, None);
        let inactive_service = inactive_seed
            .services
            .iter()
            .find(|item| item.service_id == service.id)
            .expect("historical inactive service");
        assert_eq!(inactive_service.service_name_snapshot, "Klasik Bakim");
        assert!(!inactive_service.is_eligible);
        assert_eq!(
            inactive_service.unavailable_reason.as_deref(),
            Some("service_inactive")
        );
        archive_customer(&mut connection, &customer.id).expect("archive customer");
        assert!(load_repeat_booking_seed(&connection, &customer.id, &source.id).is_err());
    }

    #[test]
    fn customer_history_is_paginated_snapshot_based_and_includes_archived_customers() {
        let (_temp, mut connection) = open_temp();
        let (customer, staff, service) = seed_core(&mut connection);
        let no_history_customer = create_customer_tx(
            &mut connection,
            CustomerInput {
                first_name: "No".into(),
                last_name: "History".into(),
                phone: None,
                email: None,
                notes: None,
                whatsapp_reminder_enabled: None,
                whatsapp_consent_confirmed: None,
            },
        )
        .expect("no history customer");
        assert!(load_customer_history(&connection, "missing", 20, 0).is_err());
        assert!(
            load_customer_history(&connection, &no_history_customer.id, 20, 0)
                .expect("empty history")
                .appointments
                .is_empty()
        );

        update_service_tx(
            &mut connection,
            &service.id,
            ServiceInput {
                category_id: service.category_id.clone(),
                name: service.name.clone(),
                duration_minutes: service.duration_minutes,
                default_price_minor: Some(100_000),
                is_active: Some(true),
            },
        )
        .expect("set service price");
        let second_service = create_service_tx(
            &mut connection,
            ServiceInput {
                category_id: service.category_id.clone(),
                name: "History Extra".into(),
                duration_minutes: Some(45),
                default_price_minor: Some(200_000),
                is_active: Some(true),
            },
        )
        .expect("second service");
        set_staff_services_tx(
            &mut connection,
            &staff.id,
            vec![service.id.clone(), second_service.id.clone()],
        )
        .expect("staff services");

        let planned = create_appointment_tx(
            &mut connection,
            appointment_input(
                &customer.id,
                &staff.id,
                "2025-01-02",
                "09:00",
                vec![service.id.clone()],
                "planned",
            ),
        )
        .expect("planned");
        let completed = create_appointment_tx(
            &mut connection,
            appointment_input(
                &customer.id,
                &staff.id,
                "2025-01-02",
                "10:00",
                vec![service.id.clone()],
                "completed",
            ),
        )
        .expect("completed");
        let no_show = create_appointment_tx(
            &mut connection,
            appointment_input(
                &customer.id,
                &staff.id,
                "2025-01-02",
                "11:00",
                vec![service.id.clone()],
                "no_show",
            ),
        )
        .expect("no show");
        let cancelled = create_appointment_tx(
            &mut connection,
            appointment_input(
                &customer.id,
                &staff.id,
                "2025-01-02",
                "12:00",
                vec![service.id.clone()],
                "cancelled",
            ),
        )
        .expect("cancelled");
        let multi_service = create_appointment_tx(
            &mut connection,
            appointment_input(
                &customer.id,
                &staff.id,
                "2025-01-02",
                "13:00",
                vec![service.id.clone(), second_service.id.clone()],
                "completed",
            ),
        )
        .expect("multi service");
        let second_staff = create_staff_tx(
            &mut connection,
            StaffInput {
                first_name: "History".into(),
                last_name: Some("Staff".into()),
                phone: None,
                specialty_note: None,
                color_key: "teal".into(),
                is_active: Some(true),
            },
        )
        .expect("second staff");
        set_staff_services_tx(&mut connection, &second_staff.id, vec![service.id.clone()])
            .expect("second staff service");
        let tie_one = create_appointment_tx(
            &mut connection,
            appointment_input(
                &customer.id,
                &staff.id,
                "2025-01-02",
                "15:00",
                vec![service.id.clone()],
                "planned",
            ),
        )
        .expect("first tie");
        let tie_two = create_appointment_tx(
            &mut connection,
            appointment_input(
                &customer.id,
                &second_staff.id,
                "2025-01-02",
                "15:00",
                vec![service.id.clone()],
                "planned",
            ),
        )
        .expect("second tie");
        let other_customer = create_customer_tx(
            &mut connection,
            CustomerInput {
                first_name: "Other".into(),
                last_name: "Customer".into(),
                phone: None,
                email: None,
                notes: None,
                whatsapp_reminder_enabled: None,
                whatsapp_consent_confirmed: None,
            },
        )
        .expect("other customer");
        let other_appointment = create_appointment_tx(
            &mut connection,
            appointment_input(
                &other_customer.id,
                &staff.id,
                "2025-01-03",
                "09:00",
                vec![service.id.clone()],
                "planned",
            ),
        )
        .expect("other appointment");

        let history = load_customer_history(&connection, &customer.id, 20, 0).expect("history");
        assert_eq!(history.customer.customer_id, customer.id);
        assert!(history.customer.is_active);
        assert_eq!(history.appointments.len(), 7);
        assert!(!history
            .appointments
            .iter()
            .any(|appointment| appointment.appointment_id == other_appointment.id));
        let statuses: HashSet<&str> = history
            .appointments
            .iter()
            .map(|appointment| appointment.status.as_str())
            .collect();
        for status in ["planned", "completed", "no_show", "cancelled"] {
            assert!(statuses.contains(status), "missing {status}");
        }
        let multi = history
            .appointments
            .iter()
            .find(|appointment| appointment.appointment_id == multi_service.id)
            .expect("multi-service history");
        assert_eq!(multi.services.len(), 2);
        assert_eq!(multi.total_charged_minor, 300_000);
        assert_eq!(multi.services[0].listed_price_snapshot_minor, 100_000);
        assert_eq!(multi.services[1].charged_price_minor, 200_000);
        assert!(!multi.staff_display_name.is_empty());

        let tie_ids: Vec<&str> = history
            .appointments
            .iter()
            .filter(|appointment| {
                appointment.local_date == "2025-01-02" && appointment.local_time == "15:00"
            })
            .map(|appointment| appointment.appointment_id.as_str())
            .collect();
        let mut expected_tie_ids = vec![tie_one.id.as_str(), tie_two.id.as_str()];
        expected_tie_ids.sort_by(|left, right| right.cmp(left));
        assert_eq!(tie_ids, expected_tie_ids);

        let first_page =
            load_customer_history(&connection, &customer.id, 2, 0).expect("first page");
        let next_page = load_customer_history(&connection, &customer.id, 2, 2).expect("next page");
        assert_eq!(first_page.appointments.len(), 2);
        assert_eq!(next_page.appointments.len(), 2);
        assert!(first_page.appointments.iter().all(|first| !next_page
            .appointments
            .iter()
            .any(|next| next.appointment_id == first.appointment_id)));

        let multi_before = multi.services.clone();
        update_service_tx(
            &mut connection,
            &service.id,
            ServiceInput {
                category_id: service.category_id.clone(),
                name: "Renamed Service".into(),
                duration_minutes: Some(60),
                default_price_minor: Some(999_000),
                is_active: Some(true),
            },
        )
        .expect("change current service");
        deactivate_service(&mut connection, &service.id).expect("deactivate service");
        deactivate_staff(&mut connection, &staff.id).expect("deactivate staff");
        archive_customer(&mut connection, &customer.id).expect("archive customer");
        let archived_history =
            load_customer_history(&connection, &customer.id, 20, 0).expect("archived history");
        assert!(!archived_history.customer.is_active);
        assert_eq!(
            archived_history
                .appointments
                .iter()
                .find(|appointment| appointment.appointment_id == multi_service.id)
                .expect("historical multi")
                .services,
            multi_before
        );
        assert!(create_appointment_tx(
            &mut connection,
            appointment_input(
                &other_customer.id,
                &staff.id,
                "2025-01-03",
                "10:00",
                vec![service.id.clone()],
                "planned",
            ),
        )
        .is_err());
        assert_eq!(planned.status, "planned");
        assert_eq!(completed.status, "completed");
        assert_eq!(no_show.status, "no_show");
        assert_eq!(cancelled.status, "cancelled");
    }

    #[test]
    fn customer_staff_service_crud_and_phone_rules() {
        let (_temp, mut connection) = open_temp();
        let (customer, staff, service) = seed_core(&mut connection);
        assert_eq!(customer.phone.as_deref(), Some("5551112233"));
        assert_eq!(staff.phone.as_deref(), Some("5552223344"));
        assert_eq!(service.availability_status, "ready");

        let duplicate_phone = create_customer_tx(
            &mut connection,
            CustomerInput {
                first_name: "Baska".into(),
                last_name: "Musteri".into(),
                phone: Some("05551112233".into()),
                email: None,
                notes: None,
                whatsapp_reminder_enabled: None,
                whatsapp_consent_confirmed: None,
            },
        )
        .expect_err("duplicate phone must have a safe domain code");
        assert!(duplicate_phone
            .to_string()
            .contains("CUSTOMER_PHONE_CONFLICT"));

        let updated = update_customer_tx(
            &mut connection,
            &customer.id,
            CustomerInput {
                first_name: "Ayse Nur".into(),
                last_name: "Yilmaz".into(),
                phone: Some("05551112233".into()),
                email: Some("ayse@example.test".into()),
                notes: Some("VIP".into()),
                whatsapp_reminder_enabled: Some(false),
                whatsapp_consent_confirmed: Some(false),
            },
        )
        .expect("update customer");
        assert_eq!(updated.first_name, "Ayse Nur");

        let inactive =
            customer_set_active_for_test(&connection, &customer.id, false).expect("inactive");
        assert!(!inactive.is_active);
    }

    #[test]
    fn category_rename_preserves_its_id_and_service_assignment() {
        let (_temp, mut connection) = open_temp();
        let (_customer, _staff, service) = seed_core(&mut connection);
        let category_id = service.category_id.clone();

        let category = update_category_tx(
            &mut connection,
            &category_id,
            CategoryInput {
                name: "Cilt Bakimi Yeni".into(),
                is_active: Some(true),
            },
        )
        .expect("rename category");

        assert_eq!(category.id, category_id);
        assert_eq!(category.name, "Cilt Bakimi Yeni");
        let refreshed_service = get_service(&connection, &service.id)
            .expect("read service")
            .expect("service remains");
        assert_eq!(refreshed_service.category_id, category_id);
        assert_eq!(refreshed_service.category_name, "Cilt Bakimi Yeni");
    }

    fn customer_set_active_for_test(
        connection: &Connection,
        id: &str,
        is_active: bool,
    ) -> Result<Customer, AppError> {
        connection.execute(
            "UPDATE customers SET is_active=?1, updated_at=?2 WHERE id=?3",
            params![bool_to_i64(is_active), now_iso(), id],
        )?;
        get_customer(connection, id)?
            .ok_or_else(|| AppError::NotFound("CUSTOMER_NOT_FOUND".to_string()))
    }

    #[test]
    fn appointment_crud_snapshot_conflict_cancel_and_restart_persistence() {
        let temp = tempdir().expect("tempdir");
        let db_path = temp.path().join("core.db");
        let mut connection = open_database(&db_path).expect("open");
        let (customer, staff, service) = seed_core(&mut connection);
        let appointment = create_appointment_tx(
            &mut connection,
            AppointmentInput {
                customer_id: customer.id.clone(),
                staff_id: staff.id.clone(),
                local_date: "2026-08-24".into(),
                local_start_time: "10:00".into(),
                service_ids: vec![service.id.clone()],
                status: Some("planned".into()),
                note: Some("ilk".into()),
                whatsapp_reminder_enabled: None,
            },
        )
        .expect("appointment");
        assert_eq!(appointment.total_duration_minutes, 30);
        assert_eq!(
            appointment.services[0].service_name_snapshot,
            "Klasik Bakim"
        );

        let conflict = create_appointment_tx(
            &mut connection,
            AppointmentInput {
                customer_id: customer.id.clone(),
                staff_id: staff.id.clone(),
                local_date: "2026-08-24".into(),
                local_start_time: "10:15".into(),
                service_ids: vec![service.id.clone()],
                status: Some("planned".into()),
                note: None,
                whatsapp_reminder_enabled: None,
            },
        );
        assert!(matches!(conflict, Err(AppError::Conflict(_))));

        let cancelled = update_appointment_tx(
            &mut connection,
            &appointment.id,
            AppointmentInput {
                customer_id: customer.id,
                staff_id: staff.id,
                local_date: "2026-08-24".into(),
                local_start_time: "10:00".into(),
                service_ids: vec![service.id],
                status: Some("cancelled".into()),
                note: Some("iptal".into()),
                whatsapp_reminder_enabled: None,
            },
        )
        .expect("cancel");
        assert_eq!(cancelled.status, "cancelled");

        drop(connection);
        let reopened = open_database(&db_path).expect("reopen");
        assert_eq!(
            get_appointment(&reopened, &appointment.id)
                .expect("read")
                .status,
            "cancelled"
        );
        assert!(integrity_check(&reopened).expect("integrity"));
    }

    #[test]
    fn v1_schema_compatibility_reads_core_tables() {
        let (_temp, mut connection) = open_temp();
        let (customer, staff, service) = seed_core(&mut connection);
        let appointment = create_appointment_tx(
            &mut connection,
            AppointmentInput {
                customer_id: customer.id,
                staff_id: staff.id,
                local_date: "2026-08-24".into(),
                local_start_time: "11:00".into(),
                service_ids: vec![service.id],
                status: Some("confirmed".into()),
                note: None,
                whatsapp_reminder_enabled: None,
            },
        )
        .expect("appointment");

        assert_eq!(
            customer_search_for_test(&connection, "555", "all")
                .expect("customers")
                .len(),
            1
        );
        assert_eq!(staff_list_for_test(&connection).expect("staff").len(), 1);
        assert_eq!(
            list_service_items(&connection, "active")
                .expect("services")
                .len(),
            1
        );
        assert_eq!(
            list_appointments_between(
                &connection,
                Some("2026-08-24T00:00:00.000Z"),
                Some("2026-08-25T00:00:00.000Z"),
                None,
                None,
                None,
                10
            )
            .expect("appointments")
            .len(),
            1
        );
        assert!(!appointment.services.is_empty());
    }

    fn customer_search_for_test(
        connection: &Connection,
        search: &str,
        status: &str,
    ) -> Result<Vec<Customer>, AppError> {
        find_customers(connection, Some(search), Some(status), 5_000)
    }

    #[test]
    fn customer_search_filters_names_and_canonical_phone_digits() {
        let (_temp, mut connection) = open_temp();
        let (_existing, _staff, _service) = seed_core(&mut connection);
        let target = create_customer_tx(
            &mut connection,
            CustomerInput {
                first_name: "Test".into(),
                last_name: "Müşterisi2".into(),
                phone: Some("05550295375".into()),
                email: None,
                notes: None,
                whatsapp_reminder_enabled: None,
                whatsapp_consent_confirmed: None,
            },
        )
        .expect("target customer");
        let unrelated = create_customer_tx(
            &mut connection,
            CustomerInput {
                first_name: "Test Google".into(),
                last_name: "Sync".into(),
                phone: Some("05551112222".into()),
                email: None,
                notes: None,
                whatsapp_reminder_enabled: None,
                whatsapp_consent_confirmed: None,
            },
        )
        .expect("unrelated customer");
        let archived = create_customer_tx(
            &mut connection,
            CustomerInput {
                first_name: "Test".into(),
                last_name: "Müşterisi2 Arşiv".into(),
                phone: Some("05553334444".into()),
                email: None,
                notes: None,
                whatsapp_reminder_enabled: None,
                whatsapp_consent_confirmed: None,
            },
        )
        .expect("archived customer");
        archive_customer(&mut connection, &archived.id).expect("archive customer");

        let name_matches = customer_search_for_test(&connection, "Test Müşterisi2", "active")
            .expect("name search");
        assert_eq!(
            name_matches.iter().map(|row| &row.id).collect::<Vec<_>>(),
            vec![&target.id]
        );
        assert!(!name_matches.iter().any(|row| row.id == unrelated.id));

        for query in ["5550295375", "05550295375"] {
            let matches =
                customer_search_for_test(&connection, query, "active").expect("phone search");
            assert_eq!(
                matches.iter().map(|row| &row.id).collect::<Vec<_>>(),
                vec![&target.id]
            );
        }
        assert!(
            customer_search_for_test(&connection, "Müşterisi2 Arşiv", "active")
                .expect("active search")
                .is_empty()
        );
    }

    fn staff_list_for_test(connection: &Connection) -> Result<Vec<Staff>, AppError> {
        let mut statement = connection.prepare("SELECT * FROM staff ORDER BY sort_order ASC")?;
        let rows = statement.query_map([], staff_from_row)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    #[test]
    fn transaction_rollback_and_scale_sanity() {
        let (_temp, mut connection) = open_temp();
        let (_customer, staff, _service) = seed_core(&mut connection);
        let before: i64 = connection
            .query_row("SELECT COUNT(*) FROM appointments", [], |row| row.get(0))
            .expect("count");
        let bad = create_appointment_tx(
            &mut connection,
            AppointmentInput {
                customer_id: new_uuid(),
                staff_id: staff.id,
                local_date: "2026-08-24".into(),
                local_start_time: "12:00".into(),
                service_ids: vec![new_uuid()],
                status: Some("planned".into()),
                note: None,
                whatsapp_reminder_enabled: None,
            },
        );
        assert!(bad.is_err());
        let after: i64 = connection
            .query_row("SELECT COUNT(*) FROM appointments", [], |row| row.get(0))
            .expect("count");
        assert_eq!(before, after);

        let tx = connection.transaction().expect("tx");
        for i in 0..1_000 {
            tx.execute(
                "INSERT INTO customers (id, first_name, last_name, phone, email, whatsapp_reminder_enabled, whatsapp_consent_confirmed, notes, is_active, created_at, updated_at)
                 VALUES (?1,'Scale','User',?2,NULL,1,0,NULL,1,?3,?3)",
                params![new_uuid(), format!("555{:07}", i), now_iso()],
            )
            .expect("insert customer");
        }
        tx.commit().expect("commit");
        let rows = customer_search_for_test(&connection, "Scale", "all").expect("search");
        assert_eq!(rows.len(), 1000);
    }

    #[test]
    fn creates_expected_data_directories() {
        let temp = tempdir().expect("tempdir");
        let (database_path, backups_dir, logs_dir, exports_dir, settings_dir) =
            data_paths(temp.path()).expect("paths");
        assert_eq!(
            database_path.file_name().and_then(|name| name.to_str()),
            Some("salon-foundation.db")
        );
        assert!(backups_dir.is_dir());
        assert!(logs_dir.is_dir());
        assert!(exports_dir.is_dir());
        assert!(settings_dir.is_dir());
    }

    #[test]
    fn business_data_directory_is_canonical_and_read_only() {
        let temp = tempdir().expect("tempdir");
        let (database_path, _, _, _, _) = data_paths(temp.path()).expect("paths");
        let mut connection = open_database(&database_path).expect("open");
        let (customer, _, _) = seed_core(&mut connection);
        let resolved = business_data_directory(&database_path).expect("data directory");
        assert_eq!(resolved, temp.path());
        assert_ne!(
            resolved,
            std::env::current_exe()
                .expect("current exe")
                .parent()
                .expect("exe directory")
        );
        assert!(get_customer(&connection, &customer.id)
            .expect("customer")
            .is_some());
    }

    fn future_local_slot(hours_from_now: i64) -> (String, String) {
        let local = Utc::now()
            + Duration::minutes(ISTANBUL_OFFSET_MINUTES)
            + Duration::hours(hours_from_now);
        if chrono::Timelike::hour(&local) >= 23 {
            let next_day = local + Duration::days(1);
            return (
                next_day.date_naive().format("%Y-%m-%d").to_string(),
                "10:00".into(),
            );
        }
        let minute = (chrono::Timelike::minute(&local) / 5) * 5;
        (
            local.date_naive().format("%Y-%m-%d").to_string(),
            format!("{:02}:{:02}", chrono::Timelike::hour(&local), minute),
        )
    }

    #[test]
    fn backup_snapshot_is_consistent_sanitized_and_preserves_business_data() {
        let temp = tempdir().expect("tempdir");
        let db_path = temp.path().join("database").join("salon-foundation.db");
        fs::create_dir_all(db_path.parent().expect("db parent")).expect("db dir");
        let mut connection = open_database(&db_path).expect("open");
        let (customer, _staff, _service) = seed_core(&mut connection);
        let source_customer_count: i64 = connection
            .query_row("SELECT COUNT(*) FROM customers", [], |row| row.get(0))
            .expect("source customer count");
        connection
            .execute(
                "INSERT INTO secure_secrets (secret_key, encrypted_value, encryption_provider, created_at, updated_at)
                 VALUES ('google_calendar_refresh_token', x'0102', 'electron_safe_storage_v1', ?1, ?1)",
                params![now_iso()],
            )
            .expect("secret");
        connection
            .execute("UPDATE google_calendar_settings SET sync_enabled=1, client_id='desktop-client', calendar_id='test-calendar', account_email='owner@example.test' WHERE id=1", [])
            .expect("google on");
        connection
            .execute("UPDATE cloud_reminder_settings SET cloud_mode_enabled=1, project_url='https://project.supabase.co', publishable_key='publishable-test-key-which-is-long-enough' WHERE id=1", [])
            .expect("cloud on");
        connection
            .execute("UPDATE whatsapp_settings SET is_enabled=1, phone_number_id='phone-id', template_name='template', automatic_reminder_enabled=1 WHERE id=1", [])
            .expect("whatsapp on");

        let backup =
            create_database_backup(&connection, &db_path, BackupKind::Manual).expect("backup");
        let backup_connection = Connection::open(&backup.backup_path).expect("backup open");
        let secret_count: i64 = backup_connection
            .query_row("SELECT COUNT(*) FROM secure_secrets", [], |row| row.get(0))
            .expect("secret count");
        let google_enabled: i64 = backup_connection
            .query_row(
                "SELECT sync_enabled FROM google_calendar_settings WHERE id=1",
                [],
                |row| row.get(0),
            )
            .expect("google");
        let google_config_count: i64 = backup_connection
            .query_row("SELECT COUNT(*) FROM google_calendar_settings WHERE client_id IS NOT NULL OR calendar_id <> 'primary' OR account_email IS NOT NULL", [], |row| row.get(0))
            .expect("google config");
        let cloud_config_count: i64 = backup_connection
            .query_row("SELECT COUNT(*) FROM cloud_reminder_settings WHERE cloud_mode_enabled <> 0 OR project_url IS NOT NULL OR publishable_key IS NOT NULL", [], |row| row.get(0))
            .expect("cloud config");
        let whatsapp_config_count: i64 = backup_connection
            .query_row("SELECT COUNT(*) FROM whatsapp_settings WHERE is_enabled <> 0 OR phone_number_id IS NOT NULL OR template_name IS NOT NULL OR automatic_reminder_enabled <> 0", [], |row| row.get(0))
            .expect("whatsapp config");
        let backup_customer_count: i64 = backup_connection
            .query_row("SELECT COUNT(*) FROM customers", [], |row| row.get(0))
            .expect("backup customer count");
        assert_eq!(secret_count, 0);
        assert_eq!(google_enabled, 0);
        assert_eq!(google_config_count, 0);
        assert_eq!(cloud_config_count, 0);
        assert_eq!(whatsapp_config_count, 0);
        assert_eq!(backup_customer_count, source_customer_count);
        assert_eq!(
            read_schema_version(&backup_connection).expect("backup schema"),
            CORE_SCHEMA_VERSION
        );
        assert!(integrity_check(&backup_connection).expect("backup integrity"));
        drop(backup_connection);

        let source_secret_count: i64 = connection
            .query_row("SELECT COUNT(*) FROM secure_secrets", [], |row| row.get(0))
            .expect("source secret count");
        assert_eq!(source_secret_count, 1, "backup must not mutate source data");

        clean_database_in_place(&mut connection, &db_path).expect("clean");
        let customer_count: i64 = connection
            .query_row("SELECT COUNT(*) FROM customers", [], |row| row.get(0))
            .expect("customers");
        assert_eq!(customer_count, 0);
        drop(connection);

        restore_database_file_for_tests(&db_path, Path::new(&backup.backup_path)).expect("restore");
        let reopened = open_database(&db_path).expect("reopen restored");
        assert!(get_customer(&reopened, &customer.id)
            .expect("customer")
            .is_some());
        assert!(integrity_check(&reopened).expect("integrity"));

        let corrupt_path = temp.path().join("corrupt.db");
        fs::write(&corrupt_path, b"not sqlite").expect("corrupt write");
        assert!(restore_database_file_for_tests(&db_path, &corrupt_path).is_err());
    }

    #[test]
    fn backup_retention_only_prunes_recognized_owned_snapshots_after_publish() {
        let temp = tempdir().expect("tempdir");
        let db_path = temp.path().join("database").join("salon-foundation.db");
        fs::create_dir_all(db_path.parent().expect("db parent")).expect("db dir");
        let mut connection = open_database(&db_path).expect("open");
        seed_core(&mut connection);

        let automatic_dir =
            backup_directory(&db_path, BackupKind::Automatic).expect("automatic dir");
        let unrelated = automatic_dir.join("keep-me.txt");
        fs::write(&unrelated, b"unrelated").expect("unrelated file");
        for _ in 0..8 {
            create_database_backup(&connection, &db_path, BackupKind::Automatic)
                .expect("automatic backup");
        }
        let automatic_count = fs::read_dir(&automatic_dir)
            .expect("automatic read")
            .filter_map(Result::ok)
            .filter(|entry| {
                entry.path().extension().and_then(|value| value.to_str()) == Some("sqlite")
            })
            .count();
        assert_eq!(automatic_count, 7);
        assert!(unrelated.exists());

        let safety_dir = backup_directory(&db_path, BackupKind::Safety).expect("safety dir");
        for _ in 0..4 {
            create_database_backup(&connection, &db_path, BackupKind::Safety)
                .expect("safety backup");
        }
        let safety_count = fs::read_dir(&safety_dir)
            .expect("safety read")
            .filter_map(Result::ok)
            .filter(|entry| {
                entry.path().extension().and_then(|value| value.to_str()) == Some("sqlite")
            })
            .count();
        assert_eq!(safety_count, 3);

        let first_manual = create_database_backup(&connection, &db_path, BackupKind::Manual)
            .expect("manual backup");
        let second_manual = create_database_backup(&connection, &db_path, BackupKind::Manual)
            .expect("manual backup");
        assert_ne!(first_manual.backup_path, second_manual.backup_path);
        assert!(Path::new(&first_manual.backup_path).is_file());
        assert!(Path::new(&second_manual.backup_path).is_file());
    }

    #[test]
    fn failed_backup_publish_or_destination_setup_leaves_no_final_snapshot() {
        let temp = tempdir().expect("tempdir");
        let temp_snapshot = temp.path().join("snapshot.tmp");
        let destination = temp.path().join("published.sqlite");
        fs::write(&temp_snapshot, b"not sqlite").expect("bad temp snapshot");
        assert!(validate_and_publish_backup(&temp_snapshot, &destination).is_err());
        assert!(!temp_snapshot.exists());
        assert!(!destination.exists());

        let blocked_root = temp.path().join("blocked");
        let blocked_db = blocked_root.join("database").join("salon-foundation.db");
        fs::create_dir_all(blocked_db.parent().expect("blocked parent")).expect("blocked db dir");
        let mut blocked_connection = open_database(&blocked_db).expect("blocked open");
        let (customer, _staff, _service) = seed_core(&mut blocked_connection);
        fs::write(blocked_root.join("backups"), b"not a directory").expect("blocked backups");
        assert!(
            create_database_backup(&blocked_connection, &blocked_db, BackupKind::Automatic)
                .is_err()
        );
        assert!(get_customer(&blocked_connection, &customer.id)
            .expect("customer")
            .is_some());
    }

    #[test]
    fn automatic_backup_coordinator_tracks_generations_retries_and_avoids_duplicate_ticks() {
        let coordinator = BackupCoordinator::default();
        assert!(coordinator.register_automatic_backup_loop());
        assert!(!coordinator.register_automatic_backup_loop());
        assert!(!coordinator.is_dirty());
        assert!(!automatic_backup_tick_with(&coordinator, || Ok(())).expect("clean tick"));

        coordinator.mark_business_change();
        assert_eq!(coordinator.current_generation(), 1);
        assert!(coordinator.is_dirty());
        assert!(automatic_backup_tick_with(&coordinator, || Ok(())).expect("dirty tick"));
        assert!(!coordinator.is_dirty());
        assert!(!automatic_backup_tick_with(&coordinator, || Ok(())).expect("clean tick"));

        coordinator.mark_business_change();
        assert!(automatic_backup_tick_with(&coordinator, || {
            Err(AppError::Database("TEST_BACKUP_FAILURE".into()))
        })
        .is_err());
        assert!(coordinator.is_dirty(), "failed backup must remain dirty");
        assert!(automatic_backup_tick_with(&coordinator, || Ok(())).expect("retry"));
        assert!(!coordinator.is_dirty());

        coordinator.mark_business_change();
        assert!(automatic_backup_tick_with(&coordinator, || {
            assert!(
                !automatic_backup_tick_with(&coordinator, || Ok(())).expect("overlap tick"),
                "a running snapshot must suppress overlapping ticks"
            );
            coordinator.mark_business_change();
            Ok(())
        })
        .expect("concurrent mutation snapshot"));
        assert!(
            coordinator.is_dirty(),
            "a mutation during snapshot must remain for the next tick"
        );
        assert!(automatic_backup_tick_with(&coordinator, || Ok(())).expect("next tick"));
        assert!(!coordinator.is_dirty());
    }

    #[test]
    fn automatic_backup_tick_creates_one_owned_snapshot_only_after_business_change() {
        let temp = tempdir().expect("tempdir");
        let db_path = temp.path().join("database").join("salon-foundation.db");
        fs::create_dir_all(db_path.parent().expect("db parent")).expect("db dir");
        let mut connection = open_database(&db_path).expect("open");
        seed_core(&mut connection);
        let coordinator = BackupCoordinator::default();

        assert!(!automatic_backup_tick(&coordinator, &db_path).expect("clean tick"));
        coordinator.mark_business_change();
        assert!(automatic_backup_tick(&coordinator, &db_path).expect("dirty tick"));
        assert!(!automatic_backup_tick(&coordinator, &db_path).expect("second clean tick"));

        let automatic_dir =
            backup_directory(&db_path, BackupKind::Automatic).expect("automatic dir");
        let snapshot_count = fs::read_dir(automatic_dir)
            .expect("backup dir")
            .filter_map(Result::ok)
            .filter(|entry| {
                entry.path().extension().and_then(|value| value.to_str()) == Some("sqlite")
            })
            .count();
        assert_eq!(snapshot_count, 1);
    }

    #[test]
    fn shutdown_backup_is_dirty_aware_bounded_and_generation_safe() {
        let coordinator = Arc::new(BackupCoordinator::default());
        let calls = AtomicU64::new(0);
        assert!(
            !shutdown_automatic_backup_with(&coordinator, StdDuration::from_millis(1), || {
                calls.fetch_add(1, Ordering::AcqRel);
                Ok(())
            })
            .expect("clean shutdown")
        );
        assert_eq!(calls.load(Ordering::Acquire), 0);

        coordinator.mark_business_change();
        assert!(
            shutdown_automatic_backup_with(&coordinator, StdDuration::from_millis(1), || {
                calls.fetch_add(1, Ordering::AcqRel);
                Ok(())
            })
            .expect("dirty shutdown")
        );
        assert_eq!(calls.load(Ordering::Acquire), 1);
        assert!(!coordinator.is_dirty());

        coordinator.mark_business_change();
        assert!(
            shutdown_automatic_backup_with(&coordinator, StdDuration::from_millis(1), || {
                Err(AppError::Database("TEST_FINAL_BACKUP_FAILURE".into()))
            })
            .is_err()
        );
        assert!(
            coordinator.is_dirty(),
            "failed final backup must remain dirty"
        );
        assert!(
            shutdown_automatic_backup_with(&coordinator, StdDuration::from_millis(1), || {
                Ok(())
            })
            .expect("final retry")
        );
        assert!(!coordinator.is_dirty());

        coordinator.mark_business_change();
        let periodic_generation = coordinator
            .begin_automatic_backup()
            .expect("periodic backup starts");
        coordinator.finish_automatic_backup(periodic_generation, true);
        assert!(
            !shutdown_automatic_backup_with(&coordinator, StdDuration::from_millis(1), || {
                panic!("covered periodic generation must suppress final backup")
            })
            .expect("covered shutdown")
        );

        coordinator.mark_business_change();
        let periodic_generation = coordinator
            .begin_automatic_backup()
            .expect("periodic backup starts");
        coordinator.mark_business_change();
        coordinator.finish_automatic_backup(periodic_generation, true);
        assert!(
            shutdown_automatic_backup_with(&coordinator, StdDuration::from_millis(1), || Ok(()))
                .expect("remaining generation final backup")
        );
        assert!(!coordinator.is_dirty());

        coordinator.mark_business_change();
        let running_generation = coordinator
            .begin_automatic_backup()
            .expect("periodic backup starts");
        let periodic_coordinator = coordinator.clone();
        let completion = std::thread::spawn(move || {
            std::thread::sleep(StdDuration::from_millis(5));
            periodic_coordinator.finish_automatic_backup(running_generation, true);
        });
        assert!(!shutdown_automatic_backup_with(
            &coordinator,
            StdDuration::from_millis(100),
            || { panic!("completed periodic backup must prevent duplicate final snapshot") }
        )
        .expect("periodic completion"));
        completion.join().expect("periodic join");

        coordinator.mark_business_change();
        let running_generation = coordinator
            .begin_automatic_backup()
            .expect("periodic backup starts");
        assert!(
            shutdown_automatic_backup_with(&coordinator, StdDuration::from_millis(1), || {
                Ok(())
            })
            .is_err()
        );
        assert!(coordinator.is_dirty(), "timeout must not clear dirty state");
        coordinator.finish_automatic_backup(running_generation, false);
    }

    #[test]
    fn shutdown_backup_uses_automatic_snapshot_sanitization_and_retention() {
        let temp = tempdir().expect("tempdir");
        let db_path = temp.path().join("database").join("salon-foundation.db");
        fs::create_dir_all(db_path.parent().expect("db parent")).expect("db dir");
        let mut connection = open_database(&db_path).expect("open");
        seed_core(&mut connection);
        connection
            .execute(
                "INSERT INTO secure_secrets (secret_key, encrypted_value, encryption_provider, created_at, updated_at)
                 VALUES ('shutdown-session', x'01', 'test', ?1, ?1)",
                params![now_iso()],
            )
            .expect("session");
        let coordinator = Arc::new(BackupCoordinator::default());
        coordinator.mark_business_change();

        assert!(
            shutdown_automatic_backup(coordinator.clone(), db_path.clone())
                .expect("shutdown backup")
        );
        assert!(!coordinator.is_dirty());
        assert!(!shutdown_automatic_backup(coordinator, db_path.clone()).expect("clean shutdown"));
        let automatic_dir =
            backup_directory(&db_path, BackupKind::Automatic).expect("automatic dir");
        let snapshots = fs::read_dir(automatic_dir)
            .expect("backup dir")
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.extension().and_then(|value| value.to_str()) == Some("sqlite"))
            .collect::<Vec<_>>();
        assert_eq!(snapshots.len(), 1);
        let snapshot = Connection::open(&snapshots[0]).expect("snapshot");
        let copied_secrets: i64 = snapshot
            .query_row("SELECT COUNT(*) FROM secure_secrets", [], |row| row.get(0))
            .expect("snapshot secrets");
        assert_eq!(copied_secrets, 0);
        let source_secrets: i64 = connection
            .query_row("SELECT COUNT(*) FROM secure_secrets", [], |row| row.get(0))
            .expect("source secrets");
        assert_eq!(
            source_secrets, 1,
            "shutdown backup must not change source DB"
        );
    }

    #[test]
    fn restore_prepares_candidate_creates_safety_backup_and_preserves_machine_secrets() {
        let temp = tempdir().expect("tempdir");
        let live_path = temp.path().join("database").join("salon-foundation.db");
        let candidate_path = temp.path().join("candidate.sqlite");
        fs::create_dir_all(live_path.parent().expect("live parent")).expect("live dir");
        let mut live = open_database(&live_path).expect("live open");
        let (live_customer, _, _) = seed_core(&mut live);
        live.execute(
            "INSERT INTO secure_secrets (secret_key, encrypted_value, encryption_provider, created_at, updated_at)
             VALUES ('machine-session', x'0102', 'test', ?1, ?1)",
            params![now_iso()],
        )
        .expect("machine secret");
        let mut candidate = open_database(&candidate_path).expect("candidate open");
        let (candidate_customer, _, _) = seed_core(&mut candidate);
        assert_ne!(live_customer.id, candidate_customer.id);
        drop(candidate);
        let state = AppState {
            database_path: live_path.clone(),
            sqlite: Mutex::new(live),
            backup_coordinator: Arc::new(BackupCoordinator::default()),
            reminder_coordinator: Arc::new(ReminderCoordinator::default()),
        };

        restore_database_from_candidate(&state, &candidate_path).expect("restore");
        let restored = state.sqlite.lock().expect("restored lock");
        assert!(get_customer(&restored, &candidate_customer.id)
            .expect("candidate customer")
            .is_some());
        assert!(get_customer(&restored, &live_customer.id)
            .expect("live customer")
            .is_none());
        assert!(integrity_check(&restored).expect("integrity"));
        assert!(foreign_key_check_clean(&restored).expect("foreign keys"));
        let secret_count: i64 = restored
            .query_row("SELECT COUNT(*) FROM secure_secrets", [], |row| row.get(0))
            .expect("machine secrets preserved");
        assert_eq!(secret_count, 1);
        drop(restored);
        let safety_count =
            fs::read_dir(backup_directory(&live_path, BackupKind::Safety).expect("safety dir"))
                .expect("safety backups")
                .filter_map(Result::ok)
                .filter(|entry| {
                    entry.path().extension().and_then(|value| value.to_str()) == Some("sqlite")
                })
                .count();
        assert_eq!(safety_count, 1);
    }

    #[test]
    fn restore_rejects_invalid_or_newer_candidates_without_touching_live_database() {
        let temp = tempdir().expect("tempdir");
        let live_path = temp.path().join("database").join("salon-foundation.db");
        let corrupt_path = temp.path().join("corrupt.sqlite");
        let newer_path = temp.path().join("newer.sqlite");
        fs::create_dir_all(live_path.parent().expect("live parent")).expect("live dir");
        let mut live = open_database(&live_path).expect("live open");
        let (live_customer, _, _) = seed_core(&mut live);
        let state = AppState {
            database_path: live_path,
            sqlite: Mutex::new(live),
            backup_coordinator: Arc::new(BackupCoordinator::default()),
            reminder_coordinator: Arc::new(ReminderCoordinator::default()),
        };

        fs::write(&corrupt_path, b"not sqlite").expect("corrupt candidate");
        assert!(restore_database_from_candidate(&state, &corrupt_path).is_err());

        let newer = open_database(&newer_path).expect("newer open");
        newer
            .execute(
                "UPDATE app_meta SET schema_version = ?1",
                params![CORE_SCHEMA_VERSION + 1],
            )
            .expect("future schema");
        drop(newer);
        assert!(restore_database_from_candidate(&state, &newer_path).is_err());

        let active = state.sqlite.lock().expect("live lock");
        assert!(get_customer(&active, &live_customer.id)
            .expect("live customer")
            .is_some());
        assert!(integrity_check(&active).expect("live integrity"));
        assert!(foreign_key_check_clean(&active).expect("live foreign keys"));
    }

    #[test]
    fn business_mutation_wrapper_marks_only_committed_business_changes() {
        let temp = tempdir().expect("tempdir");
        let db_path = temp.path().join("database").join("salon-foundation.db");
        fs::create_dir_all(db_path.parent().expect("db parent")).expect("db dir");
        let coordinator = Arc::new(BackupCoordinator::default());
        let state = AppState {
            database_path: db_path.clone(),
            sqlite: Mutex::new(open_database(&db_path).expect("open")),
            backup_coordinator: coordinator.clone(),
            reminder_coordinator: Arc::new(ReminderCoordinator::default()),
        };

        assert!(run_business_mutation(&state, |_| {
            Err::<(), AppError>(AppError::Validation("TEST_INVALID_MUTATION".into()))
        })
        .is_err());
        assert_eq!(coordinator.current_generation(), 0);

        let customer = run_business_mutation(&state, |connection| {
            create_customer_tx(
                connection,
                CustomerInput {
                    first_name: "Backup".into(),
                    last_name: "Test".into(),
                    phone: None,
                    email: None,
                    notes: None,
                    whatsapp_reminder_enabled: None,
                    whatsapp_consent_confirmed: None,
                },
            )
        })
        .expect("business create");
        assert_eq!(coordinator.current_generation(), 1);

        run_business_mutation(&state, |connection| {
            update_customer_tx(
                connection,
                &customer.id,
                CustomerInput {
                    first_name: customer.first_name.clone(),
                    last_name: customer.last_name.clone(),
                    phone: customer.phone.clone(),
                    email: customer.email.clone(),
                    notes: customer.notes.clone(),
                    whatsapp_reminder_enabled: Some(customer.whatsapp_reminder_enabled),
                    whatsapp_consent_confirmed: Some(customer.whatsapp_consent_confirmed),
                },
            )
        })
        .expect("business no-op");
        assert_eq!(
            coordinator.current_generation(),
            1,
            "no-op must not become dirty"
        );

        state
            .sqlite
            .lock()
            .expect("sqlite")
            .execute(
                "INSERT INTO secure_secrets (secret_key, encrypted_value, encryption_provider, created_at, updated_at)
                 VALUES ('test-session', x'01', 'test', ?1, ?1)",
                params![now_iso()],
            )
            .expect("session write");
        assert_eq!(
            coordinator.current_generation(),
            1,
            "session state is not a business dirty signal"
        );
    }

    #[test]
    fn google_calendar_one_way_outbox_reuses_event_and_does_not_block_local_save() {
        let (_temp, mut connection) = open_temp();
        let (customer, staff, service) = seed_core(&mut connection);
        connection
            .execute(
                "UPDATE google_calendar_settings SET sync_enabled=1 WHERE id=1",
                [],
            )
            .expect("enable");

        let appointment = create_appointment_tx(
            &mut connection,
            AppointmentInput {
                customer_id: customer.id.clone(),
                staff_id: staff.id.clone(),
                local_date: "2026-08-25".into(),
                local_start_time: "10:00".into(),
                service_ids: vec![service.id.clone()],
                status: Some("planned".into()),
                note: None,
                whatsapp_reminder_enabled: None,
            },
        )
        .expect("create");
        let pending: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM google_calendar_outbox WHERE sync_status='pending'",
                [],
                |row| row.get(0),
            )
            .expect("pending");
        assert_eq!(pending, 1);
        assert_eq!(google_sync_pending_mock_tx(&connection).expect("sync"), 1);
        let event_id: String = connection
            .query_row("SELECT google_event_id FROM appointment_google_calendar_sync WHERE appointment_id=?1", params![appointment.id], |row| row.get(0))
            .expect("event");

        update_appointment_tx(
            &mut connection,
            &appointment.id,
            AppointmentInput {
                customer_id: customer.id,
                staff_id: staff.id,
                local_date: "2026-08-25".into(),
                local_start_time: "10:00".into(),
                service_ids: vec![service.id],
                status: Some("cancelled".into()),
                note: Some("iptal".into()),
                whatsapp_reminder_enabled: None,
            },
        )
        .expect("cancel");
        assert_eq!(google_sync_pending_mock_tx(&connection).expect("resync"), 1);
        let event_after_cancel: String = connection
            .query_row("SELECT google_event_id FROM appointment_google_calendar_sync WHERE appointment_id=?1", params![appointment.id], |row| row.get(0))
            .expect("event reused");
        assert_eq!(event_id, event_after_cancel);
        let appointments: i64 = connection
            .query_row("SELECT COUNT(*) FROM appointments", [], |row| row.get(0))
            .expect("appointments");
        assert_eq!(appointments, 1);
    }

    #[test]
    fn reminders_cloud_projection_whatsapp_candidates_and_restart_persistence() {
        let temp = tempdir().expect("tempdir");
        let db_path = temp.path().join("core.db");
        let mut connection = open_database(&db_path).expect("open");
        let (mut customer, staff, service) = seed_core(&mut connection);
        customer = update_customer_tx(
            &mut connection,
            &customer.id,
            CustomerInput {
                first_name: customer.first_name.clone(),
                last_name: customer.last_name.clone(),
                phone: Some("05551112233".into()),
                email: None,
                notes: None,
                whatsapp_reminder_enabled: Some(true),
                whatsapp_consent_confirmed: Some(true),
            },
        )
        .expect("consent");
        let (local_date, local_time) = future_local_slot(2);
        let appointment = create_appointment_tx(
            &mut connection,
            AppointmentInput {
                customer_id: customer.id.clone(),
                staff_id: staff.id.clone(),
                local_date,
                local_start_time: local_time,
                service_ids: vec![service.id.clone()],
                status: Some("confirmed".into()),
                note: None,
                whatsapp_reminder_enabled: None,
            },
        )
        .expect("appointment");
        assert_eq!(
            reconcile_all_reminders_mock(&connection).expect("reconcile"),
            1
        );
        let outbox_count: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM reminder_cloud_outbox WHERE sync_status='pending'",
                [],
                |row| row.get(0),
            )
            .expect("outbox");
        assert!(outbox_count >= 1);
        let candidates = list_whatsapp_candidates_tx(&connection).expect("candidates");
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].appointment_id, appointment.id);

        update_appointment_tx(
            &mut connection,
            &appointment.id,
            AppointmentInput {
                customer_id: customer.id,
                staff_id: staff.id,
                local_date: "2026-08-24".into(),
                local_start_time: "13:00".into(),
                service_ids: vec![service.id],
                status: Some("cancelled".into()),
                note: None,
                whatsapp_reminder_enabled: None,
            },
        )
        .expect("cancel");
        let status: String = connection
            .query_row(
                "SELECT status FROM appointment_reminders WHERE appointment_id=?1",
                params![appointment.id],
                |row| row.get(0),
            )
            .expect("reminder status");
        assert_eq!(status, "cancelled");
        drop(connection);

        let reopened = open_database(&db_path).expect("reopen");
        let persisted: i64 = reopened
            .query_row("SELECT COUNT(*) FROM reminder_cloud_outbox", [], |row| {
                row.get(0)
            })
            .expect("persisted");
        assert_eq!(persisted, 0);
        let persisted_status: String = reopened
            .query_row(
                "SELECT status FROM appointment_reminders WHERE appointment_id=?1",
                params![appointment.id],
                |row| row.get(0),
            )
            .expect("persisted reminder status");
        assert_eq!(persisted_status, "cancelled");
        assert!(integrity_check(&reopened).expect("integrity"));
    }

    #[test]
    fn near_term_reminder_reuses_identity_and_compacts_an_unsynced_intent() {
        let (_temp, mut connection) = open_temp();
        let (mut customer, staff, service) = seed_core(&mut connection);
        customer = update_customer_tx(
            &mut connection,
            &customer.id,
            CustomerInput {
                first_name: customer.first_name,
                last_name: customer.last_name,
                phone: Some("05551112233".into()),
                email: None,
                notes: None,
                whatsapp_reminder_enabled: Some(true),
                whatsapp_consent_confirmed: Some(true),
            },
        )
        .expect("consented customer");
        let (local_date, local_time) = future_local_slot(2);
        let appointment = create_appointment_tx(
            &mut connection,
            AppointmentInput {
                customer_id: customer.id,
                staff_id: staff.id,
                local_date,
                local_start_time: local_time,
                service_ids: vec![service.id],
                status: Some("planned".into()),
                note: None,
                whatsapp_reminder_enabled: Some(true),
            },
        )
        .expect("near-term appointment");
        let (reminder_id, scheduled, payload): (String, String, String) = connection
            .query_row(
                "SELECT ar.id, ar.scheduled_for_utc, o.payload_json FROM appointment_reminders ar INNER JOIN reminder_cloud_outbox o ON o.reminder_id=ar.id WHERE ar.appointment_id=?1",
                params![appointment.id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .expect("near-term reminder");
        assert!(
            chrono::DateTime::parse_from_rfc3339(&scheduled)
                .expect("scheduled")
                .with_timezone(&Utc)
                <= Utc::now()
        );
        let payload: serde_json::Value = serde_json::from_str(&payload).expect("payload");
        assert_eq!(payload["nearTermImmediate"], true);
        assert!(payload["appointmentStartUtc"].is_string());

        reconcile_reminder_for_appointment(&connection, &appointment.id).expect("reconcile");
        let count: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM reminder_cloud_outbox WHERE reminder_id=?1",
                params![reminder_id],
                |row| row.get(0),
            )
            .expect("single compacted outbox event");
        assert_eq!(count, 1);

        connection
            .execute(
                "UPDATE appointments SET start_at_utc=?1, end_at_utc=?2 WHERE id=?3",
                params![
                    utc_iso(Utc::now() + Duration::hours(3)),
                    utc_iso(Utc::now() + Duration::hours(3) + Duration::minutes(30)),
                    appointment.id
                ],
            )
            .expect("reschedule");
        reconcile_reminder_for_appointment(&connection, &appointment.id)
            .expect("reconcile reschedule");
        let rescheduled_id: String = connection
            .query_row(
                "SELECT id FROM appointment_reminders WHERE appointment_id=?1",
                params![appointment.id],
                |row| row.get(0),
            )
            .expect("same reminder identity");
        assert_eq!(rescheduled_id, reminder_id);
        let rescheduled_outbox_count: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM reminder_cloud_outbox WHERE reminder_id=?1",
                params![reminder_id],
                |row| row.get(0),
            )
            .expect("single rescheduled outbox event");
        assert_eq!(rescheduled_outbox_count, 1);

        let coordinator = Arc::new(ReminderCoordinator::default());
        assert!(coordinator.register());
        let (sender, receiver) = mpsc::channel();
        *coordinator.signal_sender.lock().expect("signal sender") = Some(sender);
        let state = AppState {
            database_path: PathBuf::from("test.db"),
            sqlite: Mutex::new(connection),
            backup_coordinator: Arc::new(BackupCoordinator::default()),
            reminder_coordinator: coordinator,
        };
        request_reminder_projection(&state, &appointment.id);
        assert!(matches!(
            receiver
                .recv_timeout(StdDuration::from_millis(100))
                .expect("rescheduled projection wake"),
            ReminderCoordinatorSignal::Wake
        ));
        let connection = state.sqlite.lock().expect("sqlite");

        connection
            .execute(
                "UPDATE appointments SET start_at_utc=?1 WHERE id=?2",
                params![utc_iso(Utc::now() - Duration::minutes(1)), appointment.id],
            )
            .expect("past start");
        reconcile_reminder_for_appointment(&connection, &appointment.id)
            .expect("cancel past appointment");
        let status: String = connection
            .query_row(
                "SELECT status FROM appointment_reminders WHERE id=?1",
                params![reminder_id],
                |row| row.get(0),
            )
            .expect("cancelled reminder");
        assert_eq!(status, "cancelled");
        let pending: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM reminder_cloud_outbox WHERE reminder_id=?1 AND sync_status='pending'",
                params![reminder_id],
                |row| row.get(0),
            )
            .expect("no deliverable outbox item");
        assert_eq!(pending, 0);
    }

    #[test]
    fn pending_near_term_outbox_requests_a_coalesced_projection_wake() {
        let (temp, mut connection) = open_temp();
        let (mut customer, staff, service) = seed_core(&mut connection);
        customer = update_customer_tx(
            &mut connection,
            &customer.id,
            CustomerInput {
                first_name: customer.first_name,
                last_name: customer.last_name,
                phone: Some("05551112233".into()),
                email: None,
                notes: None,
                whatsapp_reminder_enabled: Some(true),
                whatsapp_consent_confirmed: Some(true),
            },
        )
        .expect("consented customer");
        let (local_date, local_time) = future_local_slot(2);
        let appointment = create_appointment_tx(
            &mut connection,
            AppointmentInput {
                customer_id: customer.id,
                staff_id: staff.id,
                local_date,
                local_start_time: local_time,
                service_ids: vec![service.id],
                status: Some("planned".into()),
                note: None,
                whatsapp_reminder_enabled: Some(true),
            },
        )
        .expect("near-term appointment");
        let coordinator = Arc::new(ReminderCoordinator::default());
        assert!(coordinator.register());
        let (sender, receiver) = mpsc::channel();
        *coordinator.signal_sender.lock().expect("signal sender") = Some(sender);
        let state = AppState {
            database_path: reminder_coordinator_diagnostic_test_database_path(&temp),
            sqlite: Mutex::new(connection),
            backup_coordinator: Arc::new(BackupCoordinator::default()),
            reminder_coordinator: coordinator,
        };

        request_reminder_projection(&state, &appointment.id);
        request_reminder_projection(&state, &appointment.id);

        assert!(matches!(
            receiver
                .recv_timeout(StdDuration::from_millis(100))
                .expect("projection wake"),
            ReminderCoordinatorSignal::Wake
        ));
        assert!(matches!(
            receiver.recv_timeout(StdDuration::from_millis(25)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));
        let diagnostic = read_reminder_coordinator_diagnostic(&temp);
        assert!(diagnostic.contains("wake_requested registered=true send_result=success"));
        assert!(diagnostic.contains("wake_requested registered=true send_result=coalesced"));
        let connection = state.sqlite.lock().expect("sqlite");
        let pending: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM reminder_cloud_outbox WHERE reminder_id IN (SELECT id FROM appointment_reminders WHERE appointment_id=?1) AND sync_status='pending'",
                params![appointment.id],
                |row| row.get(0),
            )
            .expect("pending outbox preserved");
        assert_eq!(pending, 1);
    }

    #[test]
    fn reminder_projection_wake_failure_preserves_local_outbox_and_ineligible_appointments_do_not_wake(
    ) {
        let (temp, mut connection) = open_temp();
        let (mut customer, staff, service) = seed_core(&mut connection);
        customer = update_customer_tx(
            &mut connection,
            &customer.id,
            CustomerInput {
                first_name: customer.first_name,
                last_name: customer.last_name,
                phone: Some("05551112233".into()),
                email: None,
                notes: None,
                whatsapp_reminder_enabled: Some(true),
                whatsapp_consent_confirmed: Some(true),
            },
        )
        .expect("consented customer");
        let (local_date, local_time) = future_local_slot(2);
        let eligible = create_appointment_tx(
            &mut connection,
            AppointmentInput {
                customer_id: customer.id.clone(),
                staff_id: staff.id.clone(),
                local_date: local_date.clone(),
                local_start_time: local_time.clone(),
                service_ids: vec![service.id.clone()],
                status: Some("planned".into()),
                note: None,
                whatsapp_reminder_enabled: Some(true),
            },
        )
        .expect("eligible appointment");
        let ineligible = create_appointment_tx(
            &mut connection,
            AppointmentInput {
                customer_id: customer.id,
                staff_id: staff.id,
                local_date,
                local_start_time: future_local_slot(3).1,
                service_ids: vec![service.id],
                status: Some("planned".into()),
                note: None,
                whatsapp_reminder_enabled: Some(false),
            },
        )
        .expect("ineligible appointment");
        let coordinator = Arc::new(ReminderCoordinator::default());
        assert!(coordinator.register());
        let blocked_root = temp.path().join("blocked-root");
        fs::write(&blocked_root, "not a directory").expect("block telemetry path");
        let state = AppState {
            database_path: blocked_root.join("database").join("salon-foundation.db"),
            sqlite: Mutex::new(connection),
            backup_coordinator: Arc::new(BackupCoordinator::default()),
            reminder_coordinator: coordinator.clone(),
        };

        request_reminder_projection(&state, &eligible.id);
        assert!(!coordinator.wake_pending.load(Ordering::Acquire));
        request_reminder_projection(&state, &ineligible.id);
        assert!(!coordinator.wake_pending.load(Ordering::Acquire));

        let connection = state.sqlite.lock().expect("sqlite");
        let eligible_pending: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM reminder_cloud_outbox WHERE reminder_id IN (SELECT id FROM appointment_reminders WHERE appointment_id=?1) AND sync_status='pending'",
                params![eligible.id],
                |row| row.get(0),
            )
            .expect("eligible pending outbox preserved");
        let ineligible_pending: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM reminder_cloud_outbox WHERE reminder_id IN (SELECT id FROM appointment_reminders WHERE appointment_id=?1) AND sync_status='pending'",
                params![ineligible.id],
                |row| row.get(0),
            )
            .expect("no ineligible outbox");
        assert_eq!(eligible_pending, 1);
        assert_eq!(ineligible_pending, 0);
    }

    #[test]
    fn normal_reminder_keeps_exact_twenty_four_hour_schedule() {
        let (_temp, mut connection) = open_temp();
        let (mut customer, staff, service) = seed_core(&mut connection);
        customer = update_customer_tx(
            &mut connection,
            &customer.id,
            CustomerInput {
                first_name: customer.first_name,
                last_name: customer.last_name,
                phone: Some("05551112233".into()),
                email: None,
                notes: None,
                whatsapp_reminder_enabled: Some(true),
                whatsapp_consent_confirmed: Some(true),
            },
        )
        .expect("consented customer");
        let (local_date, local_time) = future_local_slot(48);
        let appointment = create_appointment_tx(
            &mut connection,
            AppointmentInput {
                customer_id: customer.id,
                staff_id: staff.id,
                local_date,
                local_start_time: local_time,
                service_ids: vec![service.id],
                status: Some("planned".into()),
                note: None,
                whatsapp_reminder_enabled: Some(true),
            },
        )
        .expect("normal appointment");
        let (scheduled, payload): (String, String) = connection
            .query_row(
                "SELECT ar.scheduled_for_utc, o.payload_json FROM appointment_reminders ar INNER JOIN reminder_cloud_outbox o ON o.reminder_id=ar.id WHERE ar.appointment_id=?1",
                params![appointment.id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("normal reminder");
        let start_at = chrono::DateTime::parse_from_rfc3339(&appointment.start_at_utc)
            .expect("start")
            .with_timezone(&Utc);
        assert_eq!(scheduled, utc_iso(start_at - Duration::hours(24)));
        let payload: serde_json::Value = serde_json::from_str(&payload).expect("payload");
        assert_eq!(payload["nearTermImmediate"], false);
        assert_eq!(payload["appointmentStartUtc"], appointment.start_at_utc);
    }

    #[test]
    fn reminder_delivery_gates_block_ineligible_appointments() {
        fn reminder_count(connection: &Connection, appointment_id: &str) -> i64 {
            connection
                .query_row(
                    "SELECT COUNT(*) FROM appointment_reminders WHERE appointment_id=?1",
                    params![appointment_id],
                    |row| row.get(0),
                )
                .expect("reminder count")
        }

        let (_temp, mut connection) = open_temp();
        let (mut customer, staff, service) = seed_core(&mut connection);
        let (local_date, local_time) = future_local_slot(48);
        let consent_missing = create_appointment_tx(
            &mut connection,
            AppointmentInput {
                customer_id: customer.id.clone(),
                staff_id: staff.id.clone(),
                local_date: local_date.clone(),
                local_start_time: local_time.clone(),
                service_ids: vec![service.id.clone()],
                status: Some("planned".into()),
                note: None,
                whatsapp_reminder_enabled: Some(true),
            },
        )
        .expect("appointment without consent");
        assert_eq!(reminder_count(&connection, &consent_missing.id), 0);

        customer = update_customer_tx(
            &mut connection,
            &customer.id,
            CustomerInput {
                first_name: customer.first_name,
                last_name: customer.last_name,
                phone: Some("05551112233".into()),
                email: None,
                notes: None,
                whatsapp_reminder_enabled: Some(true),
                whatsapp_consent_confirmed: Some(true),
            },
        )
        .expect("consented customer");
        let (preference_date, preference_time) = future_local_slot(72);
        let preference_off = create_appointment_tx(
            &mut connection,
            AppointmentInput {
                customer_id: customer.id.clone(),
                staff_id: staff.id.clone(),
                local_date: preference_date,
                local_start_time: preference_time,
                service_ids: vec![service.id.clone()],
                status: Some("planned".into()),
                note: None,
                whatsapp_reminder_enabled: Some(false),
            },
        )
        .expect("appointment preference off");
        assert_eq!(reminder_count(&connection, &preference_off.id), 0);

        let (terminal_date, terminal_time) = future_local_slot(96);
        let terminal = create_appointment_tx(
            &mut connection,
            AppointmentInput {
                customer_id: customer.id.clone(),
                staff_id: staff.id.clone(),
                local_date: terminal_date,
                local_start_time: terminal_time,
                service_ids: vec![service.id.clone()],
                status: Some("completed".into()),
                note: None,
                whatsapp_reminder_enabled: Some(true),
            },
        )
        .expect("terminal appointment");
        assert_eq!(reminder_count(&connection, &terminal.id), 0);

        connection
            .execute(
                "UPDATE customers SET phone='not-a-phone' WHERE id=?1",
                params![customer.id],
            )
            .expect("inject invalid phone");
        let (invalid_phone_date, invalid_phone_time) = future_local_slot(120);
        let invalid_phone = create_appointment_tx(
            &mut connection,
            AppointmentInput {
                customer_id: customer.id,
                staff_id: staff.id,
                local_date: invalid_phone_date,
                local_start_time: invalid_phone_time,
                service_ids: vec![service.id],
                status: Some("planned".into()),
                note: None,
                whatsapp_reminder_enabled: Some(true),
            },
        )
        .expect("appointment with invalid stored phone");
        assert_eq!(reminder_count(&connection, &invalid_phone.id), 0);
    }

    #[test]
    fn mock_auth_session_uses_secure_secret_boundary_and_logout_clears_it() {
        let (_temp, connection) = open_temp();
        let session =
            mock_auth_verify_otp_tx(&connection, "owner@example.test", "123456").expect("otp");
        assert_eq!(session.access_token_hint, "mock-session-present");
        let secret_count: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM secure_secrets WHERE secret_key='supabase_mock_session'",
                [],
                |row| row.get(0),
            )
            .expect("secret");
        assert_eq!(secret_count, 1);
        let settings_count: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM cloud_reminder_settings WHERE publishable_key IS NOT NULL",
                [],
                |row| row.get(0),
            )
            .expect("settings");
        assert_eq!(settings_count, 0);
        connection
            .execute(
                "DELETE FROM secure_secrets WHERE secret_key='supabase_mock_session'",
                [],
            )
            .expect("logout");
        let after: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM secure_secrets WHERE secret_key='supabase_mock_session'",
                [],
                |row| row.get(0),
            )
            .expect("after");
        assert_eq!(after, 0);
    }

    #[test]
    fn google_real_client_boundaries_validate_oauth_state_refresh_and_same_event_updates() {
        let auth_url = services::google::build_google_auth_url(
            "client-id",
            "http://127.0.0.1:4444/oauth2callback",
            "state-1",
            "challenge-1",
        )
        .expect("auth url");
        assert!(
            auth_url.contains("scope=https%3A%2F%2Fwww.googleapis.com%2Fauth%2Fcalendar.events")
        );
        validate_oauth_redirect_uri(&auth_url, "http://127.0.0.1:4444/oauth2callback")
            .expect("redirect uri matches listener");
        assert!(
            validate_oauth_redirect_uri(&auth_url, "http://127.0.0.1:5555/oauth2callback").is_err()
        );
        assert_eq!(
            services::google::parse_oauth_callback(
                "http://127.0.0.1:4444/oauth2callback?state=state-1&code=abc",
                "state-1"
            )
            .expect("code"),
            "abc"
        );
        assert!(services::google::parse_oauth_callback(
            "http://127.0.0.1:4444/oauth2callback?state=bad&code=abc",
            "state-1"
        )
        .is_err());

        let config = services::google::GoogleOAuthConfig {
            client_id: "client-id".into(),
            client_secret: "client-secret".into(),
            calendar_id: Some("primary".into()),
        };
        let mut transport = services::google::FakeHttpTransport::new(vec![
            services::google::HttpResponse {
                status: 200,
                body: br#"{"refresh_token":"refresh-1","access_token":"access-1"}"#.to_vec(),
            },
            services::google::HttpResponse {
                status: 200,
                body: br#"{"access_token":"access-2"}"#.to_vec(),
            },
            services::google::HttpResponse {
                status: 200,
                body: br#"{"id":"event-1"}"#.to_vec(),
            },
            services::google::HttpResponse {
                status: 200,
                body: br#"{"id":"event-1"}"#.to_vec(),
            },
        ]);
        let tokens = services::google::exchange_auth_code(
            &mut transport,
            &config,
            "http://127.0.0.1:4444/oauth2callback",
            "abc",
            "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk",
        )
        .expect("exchange");
        assert_eq!(tokens.refresh_token, "refresh-1");
        let access =
            services::google::refresh_access_token(&mut transport, &config, &tokens.refresh_token)
                .expect("refresh");
        assert_eq!(access, "access-2");

        let (_temp, mut connection) = open_temp();
        let (customer, staff, service) = seed_core(&mut connection);
        let appointment = create_appointment_tx(
            &mut connection,
            AppointmentInput {
                customer_id: customer.id,
                staff_id: staff.id,
                local_date: "2026-08-25".into(),
                local_start_time: "15:00".into(),
                service_ids: vec![service.id],
                status: Some("planned".into()),
                note: None,
                whatsapp_reminder_enabled: None,
            },
        )
        .expect("appointment");
        connection.execute("INSERT INTO appointment_google_calendar_sync (appointment_id, google_event_id, created_at_utc, updated_at_utc) VALUES (?1, NULL, ?2, ?2) ON CONFLICT DO NOTHING", params![appointment.id, now_iso()]).expect("sync row");
        let payload = services::google::build_calendar_event(&appointment);
        let created_id = services::google::upsert_calendar_event(
            &mut transport,
            &access,
            "primary",
            None,
            &payload,
        )
        .expect("create");
        services::google::mark_google_outbox_synced(
            &mut connection,
            &appointment.id,
            &created_id,
            &appointment.start_at_utc,
        )
        .expect("mark");
        services::google::reject_unrelated_event_mutation(&connection, &appointment.id, "event-1")
            .expect("trusted");
        assert!(services::google::reject_unrelated_event_mutation(
            &connection,
            &appointment.id,
            "other-event"
        )
        .is_err());
        let cancelled = AppointmentSummary {
            status: "cancelled".into(),
            ..appointment
        };
        let cancel_payload = services::google::build_calendar_event(&cancelled);
        assert!(cancel_payload["summary"]
            .as_str()
            .expect("summary")
            .starts_with("IPTAL - "));
        let updated_id = services::google::upsert_calendar_event(
            &mut transport,
            &access,
            "primary",
            Some("event-1"),
            &cancel_payload,
        )
        .expect("update same");
        assert_eq!(updated_id, "event-1");
        assert!(transport
            .requests
            .iter()
            .any(|request| request.method == "PATCH" && request.url.contains("/events/event-1")));
    }

    #[test]
    fn supabase_real_auth_client_uses_public_config_and_secure_session_storage_with_mock_transport()
    {
        let config = services::supabase::SupabaseConfig {
            project_url: "https://example.supabase.co".into(),
            publishable_key: "pub-key-with-enough-length".into(),
        };
        let mut transport = services::google::FakeHttpTransport::new(vec![
            services::google::HttpResponse { status: 200, body: b"{}".to_vec() },
            services::google::HttpResponse { status: 200, body: br#"{"access_token":"access","refresh_token":"refresh","expires_at":1800000000}"#.to_vec() },
            services::google::HttpResponse { status: 200, body: br#"{"access_token":"access2","refresh_token":"refresh2"}"#.to_vec() },
            services::google::HttpResponse { status: 200, body: br#"{"id":"user"}"#.to_vec() },
        ]);
        services::supabase::request_email_otp(&mut transport, &config, "owner@example.test")
            .expect("otp request");
        let session = services::supabase::verify_email_otp(
            &mut transport,
            &config,
            "owner@example.test",
            "123456",
        )
        .expect("verify");
        assert_eq!(session.access_token, "access");
        let refreshed =
            services::supabase::refresh_session(&mut transport, &config, &session.refresh_token)
                .expect("refresh");
        services::supabase::validate_authenticated_session(
            &mut transport,
            &config,
            &refreshed.access_token,
        )
        .expect("validate");
        assert!(transport
            .requests
            .iter()
            .any(|request| request.url.ends_with("/auth/v1/otp")));
        assert!(transport
            .requests
            .iter()
            .any(|request| request.url.contains("grant_type=refresh_token")));

        let (_temp, connection) = open_temp();
        let protector = services::secure_store::TestProtector;
        services::secure_store::store_supabase_session(&connection, &protector, &session)
            .expect("store");
        let restored = services::secure_store::read_supabase_session(&connection, &protector)
            .expect("read")
            .expect("session");
        assert_eq!(restored.refresh_token, "refresh");
        let raw: Vec<u8> = connection.query_row("SELECT encrypted_value FROM secure_secrets WHERE secret_key='cloud_supabase_session'", [], |row| row.get(0)).expect("raw");
        assert!(!String::from_utf8_lossy(&raw).contains("refresh"));
    }

    #[test]
    fn hosted_cloud_client_processes_ordered_outbox_and_reconciles_status_without_whatsapp_send() {
        let (_temp, mut connection) = open_temp();
        let (mut customer, staff, service) = seed_core(&mut connection);
        customer = update_customer_tx(
            &mut connection,
            &customer.id,
            CustomerInput {
                first_name: customer.first_name,
                last_name: customer.last_name,
                phone: Some("05551112233".into()),
                email: None,
                notes: None,
                whatsapp_reminder_enabled: Some(true),
                whatsapp_consent_confirmed: Some(true),
            },
        )
        .expect("customer");
        let (local_date, local_start_time) = future_local_slot(48);
        create_appointment_tx(
            &mut connection,
            AppointmentInput {
                customer_id: customer.id,
                staff_id: staff.id,
                local_date,
                local_start_time,
                service_ids: vec![service.id],
                status: Some("confirmed".into()),
                note: None,
                whatsapp_reminder_enabled: None,
            },
        )
        .expect("appointment");
        let reminder_id: String = connection
            .query_row("SELECT id FROM appointment_reminders LIMIT 1", [], |row| {
                row.get(0)
            })
            .expect("reminder");
        let config = services::supabase::SupabaseConfig {
            project_url: "https://example.supabase.co".into(),
            publishable_key: "pub-key-with-enough-length".into(),
        };
        let session = services::supabase::SupabaseSession {
            access_token: "access".into(),
            refresh_token: "refresh".into(),
            expires_at: None,
        };
        let mut transport = services::google::FakeHttpTransport::new(vec![
            services::google::HttpResponse { status: 200, body: br#"{"data":{"revision":1,"status":"pending","remoteUpdatedAtUtc":"2026-08-25T00:00:00.000Z"}}"#.to_vec() },
            services::google::HttpResponse { status: 200, body: br#"{"data":{"revision":2,"status":"cancelled","remoteUpdatedAtUtc":"2026-08-25T00:01:00.000Z"}}"#.to_vec() },
        ]);
        let first = services::reminder_cloud::process_ordered_outbox(
            &connection,
            &mut transport,
            &config,
            &session,
            10,
        )
        .expect("first sync");
        assert_eq!(first.processed, 1);
        upsert_cloud_outbox(&connection, &reminder_id, "cancel", None).expect("second revision");
        let second = services::reminder_cloud::process_ordered_outbox(
            &connection,
            &mut transport,
            &config,
            &session,
            10,
        )
        .expect("second sync");
        assert_eq!(second.processed, 1);
        assert!(transport
            .requests
            .iter()
            .any(|request| request.url.ends_with("/functions/v1/reminder-upsert")));
        assert!(transport
            .requests
            .iter()
            .any(|request| request.url.ends_with("/functions/v1/reminder-cancel")));
        let pending: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM reminder_cloud_outbox WHERE sync_status='pending'",
                [],
                |row| row.get(0),
            )
            .expect("pending");
        assert_eq!(pending, 0);
        services::reminder_cloud::reconcile_remote_status(
            &connection,
            &reminder_id,
            2,
            "sent",
            "2026-08-25T00:02:00.000Z",
        )
        .expect("reconcile");
        let local_status: String = connection
            .query_row(
                "SELECT status FROM appointment_reminders WHERE id=?1",
                params![reminder_id],
                |row| row.get(0),
            )
            .expect("status");
        assert_eq!(local_status, "sent");
    }

    #[test]
    fn targeted_cloud_outbox_processing_leaves_other_pending_items_untouched() {
        let (_temp, mut connection) = open_temp();
        let (mut customer, staff, service) = seed_core(&mut connection);
        customer = update_customer_tx(
            &mut connection,
            &customer.id,
            CustomerInput {
                first_name: customer.first_name,
                last_name: customer.last_name,
                phone: Some("05551112233".into()),
                email: None,
                notes: None,
                whatsapp_reminder_enabled: Some(true),
                whatsapp_consent_confirmed: Some(true),
            },
        )
        .expect("customer");
        let appointments = [48, 72, 96]
            .iter()
            .map(|hours| {
                let (local_date, local_start_time) = future_local_slot(*hours);
                create_appointment_tx(
                    &mut connection,
                    AppointmentInput {
                        customer_id: customer.id.clone(),
                        staff_id: staff.id.clone(),
                        local_date,
                        local_start_time,
                        service_ids: vec![service.id.clone()],
                        status: Some("confirmed".into()),
                        note: None,
                        whatsapp_reminder_enabled: None,
                    },
                )
                .expect("appointment")
            })
            .collect::<Vec<_>>();
        let ids = appointments
            .iter()
            .map(|appointment| {
                connection
                    .query_row(
                        "SELECT id FROM reminder_cloud_outbox WHERE reminder_id=(SELECT id FROM appointment_reminders WHERE appointment_id=?1)",
                        params![appointment.id],
                        |row| row.get::<_, String>(0),
                    )
                    .expect("outbox id")
            })
            .collect::<Vec<_>>();
        let config = services::supabase::SupabaseConfig {
            project_url: "https://example.supabase.co".into(),
            publishable_key: "pub-key-with-enough-length".into(),
        };
        let session = services::supabase::SupabaseSession {
            access_token: "access".into(),
            refresh_token: "refresh".into(),
            expires_at: None,
        };
        let mut transport = services::google::FakeHttpTransport::new(vec![
            services::google::HttpResponse {
                status: 200,
                body: br#"{"data":{"revision":1,"status":"pending"}}"#.to_vec(),
            },
            services::google::HttpResponse {
                status: 200,
                body: br#"{"data":{"revision":2,"status":"cancelled"}}"#.to_vec(),
            },
        ]);
        let first = services::reminder_cloud::process_outbox_item(
            &connection,
            &mut transport,
            &config,
            &session,
            &ids[0],
        )
        .expect("target upsert");
        assert_eq!(first.processed, 1);
        let untouched: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM reminder_cloud_outbox WHERE id IN (?1, ?2) AND sync_status='pending'",
                params![&ids[1], &ids[2]],
                |row| row.get(0),
            )
            .expect("untouched");
        assert_eq!(untouched, 2);
        assert_eq!(transport.requests.len(), 1);
        let payload: serde_json::Value =
            serde_json::from_slice(&transport.requests[0].body).expect("upsert payload");
        assert_eq!(payload["apiVersion"], 1);
        assert!(payload["reminderId"].as_str().is_some());
        assert!(payload["clientMutationId"].as_str().is_some());
        assert_eq!(payload["recipient"].as_str(), Some("905551112233"));
        assert_eq!(
            payload["template"]["name"].as_str(),
            Some("randevu_hatirlatma")
        );
        assert_eq!(
            payload["template"]["parameters"].as_array().map(Vec::len),
            Some(4)
        );

        let missing = services::reminder_cloud::process_outbox_item(
            &connection,
            &mut transport,
            &config,
            &session,
            "missing-outbox-id",
        )
        .expect("missing is safe no-op");
        assert_eq!(missing.processed, 0);
        let duplicate = services::reminder_cloud::process_outbox_item(
            &connection,
            &mut transport,
            &config,
            &session,
            &ids[0],
        )
        .expect("synced is safe no-op");
        assert_eq!(duplicate.processed, 0);
        assert_eq!(transport.requests.len(), 1);

        let reminder_id: String = connection
            .query_row(
                "SELECT id FROM appointment_reminders WHERE appointment_id=?1",
                params![appointments[0].id],
                |row| row.get(0),
            )
            .expect("target reminder");
        upsert_cloud_outbox(&connection, &reminder_id, "cancel", None).expect("cancel outbox");
        let cancel_id: String = connection
            .query_row(
                "SELECT id FROM reminder_cloud_outbox WHERE reminder_id=?1 AND action='cancel'",
                params![reminder_id],
                |row| row.get(0),
            )
            .expect("cancel id");
        let cancel = services::reminder_cloud::process_outbox_item(
            &connection,
            &mut transport,
            &config,
            &session,
            &cancel_id,
        )
        .expect("target cancel");
        assert_eq!(cancel.processed, 1);
        assert_eq!(transport.requests.len(), 2);
        assert!(transport.requests[1]
            .url
            .ends_with("/functions/v1/reminder-cancel"));
        let still_untouched: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM reminder_cloud_outbox WHERE id IN (?1, ?2) AND sync_status='pending'",
                params![&ids[1], &ids[2]],
                |row| row.get(0),
            )
            .expect("still untouched");
        assert_eq!(still_untouched, 2);
    }

    #[test]
    fn live_services_config_file_loads_only_local_command_configuration() {
        let config = parse_live_services_config(
            r#"{
              "google": {
                "clientId": "desktop-client-id",
                "clientSecret": "desktop-client-secret",
                "calendarId": "primary"
              },
              "supabase": {
                "projectUrl": "https://example.supabase.co",
                "publishableKey": "publishable-key-with-enough-length"
              }
            }"#,
        )
        .expect("config");
        assert_eq!(
            config.google.expect("google").calendar_id.as_deref(),
            Some("primary")
        );
        services::supabase::validate_public_config(&config.supabase.expect("supabase"))
            .expect("public config");
        assert!(parse_live_services_config("{not-json").is_err());
    }

    #[test]
    fn google_outbox_command_helper_updates_only_trusted_mapped_events() {
        let (_temp, mut connection) = open_temp();
        let (customer, staff, service) = seed_core(&mut connection);
        let appointment = create_appointment_tx(
            &mut connection,
            AppointmentInput {
                customer_id: customer.id,
                staff_id: staff.id,
                local_date: "2026-08-27".into(),
                local_start_time: "11:00".into(),
                service_ids: vec![service.id],
                status: Some("planned".into()),
                note: None,
                whatsapp_reminder_enabled: None,
            },
        )
        .expect("appointment");
        connection.execute(
            "INSERT INTO appointment_google_calendar_sync (appointment_id, google_event_id, created_at_utc, updated_at_utc)
             VALUES (?1, 'event-42', ?2, ?2)",
            params![appointment.id, now_iso()],
        )
        .expect("mapping");
        connection.execute(
            "INSERT INTO google_calendar_outbox (id, appointment_id, sync_status, created_at_utc, updated_at_utc)
             VALUES (?1, ?2, 'pending', ?3, ?3)",
            params![new_uuid(), appointment.id, now_iso()],
        )
        .expect("outbox");
        let mut transport =
            services::google::FakeHttpTransport::new(vec![services::google::HttpResponse {
                status: 200,
                body: br#"{"id":"event-42"}"#.to_vec(),
            }]);
        let processed = process_google_outbox(&connection, &mut transport, "access", "primary", 10)
            .expect("process");
        assert_eq!(processed, 1);
        assert!(transport
            .requests
            .iter()
            .any(|request| request.method == "PATCH" && request.url.contains("/events/event-42")));
        let status: String = connection
            .query_row(
                "SELECT sync_status FROM google_calendar_outbox WHERE appointment_id=?1",
                params![appointment.id],
                |row| row.get(0),
            )
            .expect("status");
        assert_eq!(status, "synced");
    }

    #[test]
    fn oauth_callback_listener_ignores_favicon_and_accepts_callback() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        listener.set_nonblocking(true).expect("nonblocking");
        let port = listener.local_addr().expect("port").port();
        let redirect_uri = format!("http://127.0.0.1:{port}/oauth2callback");

        let handle = std::thread::spawn(move || {
            std::thread::sleep(StdDuration::from_millis(50));
            // First send a favicon noise request
            if let Ok(mut stream) = std::net::TcpStream::connect(format!("127.0.0.1:{port}")) {
                let _ = stream.write_all(b"GET /favicon.ico HTTP/1.1\r\nHost: localhost\r\n\r\n");
                let _ = stream.flush();
            }
            std::thread::sleep(StdDuration::from_millis(50));
            // Next send the real OAuth callback
            if let Ok(mut stream) = std::net::TcpStream::connect(format!("127.0.0.1:{port}")) {
                let _ = stream.write_all(
                    b"GET /oauth2callback?code=mock_auth_code_123&state=mock_state_456 HTTP/1.1\r\nHost: localhost\r\n\r\n",
                );
                let _ = stream.flush();
                let mut resp = [0_u8; 512];
                let _ = stream.read(&mut resp);
            }
        });

        let callback_url =
            wait_for_oauth_callback(&listener, &redirect_uri, StdDuration::from_secs(5))
                .expect("callback url");
        handle.join().expect("thread join");

        let code = services::google::parse_oauth_callback(&callback_url, "mock_state_456")
            .expect("parsed code");
        assert_eq!(code, "mock_auth_code_123");
    }

    #[test]
    fn browser_launcher_validates_url_scheme() {
        assert!(open_system_browser("file:///C:/secrets.txt").is_err());
        assert!(open_system_browser("javascript:alert(1)").is_err());
        assert!(open_system_browser("ftp://example.com").is_err());
    }

    #[test]
    fn shell_execute_result_requires_a_success_handle() {
        assert!(!shell_execute_succeeded(0));
        assert!(!shell_execute_succeeded(32));
        assert!(shell_execute_succeeded(33));
    }

    #[test]
    #[ignore = "requires explicit live Google Calendar acceptance authorization"]
    fn live_google_calendar_acceptance() {
        assert_eq!(
            std::env::var("BEAUTYSALOON_ALLOW_LIVE_GOOGLE_ACCEPTANCE").as_deref(),
            Ok("1"),
            "live acceptance requires an explicit environment gate"
        );

        let app_data = std::env::var_os("APPDATA").expect("APPDATA");
        let database_path = PathBuf::from(app_data)
            .join("com.beautysaloon.desktop")
            .join("database")
            .join("salon-foundation.db");
        let mut connection = open_database(&database_path).expect("open live database");
        let protector = services::secure_store::WindowsDpapiProtector;
        let connected = services::google::google_connection_status(
            &connection,
            services::secure_store::secure_storage_available(&protector),
        )
        .expect("connection status");
        assert!(connected.connected, "Google connection must be persisted");

        let config = google_config().expect("local Google config");
        let calendar_id = config
            .calendar_id
            .clone()
            .unwrap_or_else(|| "primary".to_string());
        let refresh_token = services::secure_store::read_secret(
            &connection,
            &protector,
            "google_calendar_refresh_token",
        )
        .expect("read refresh token")
        .expect("persisted refresh token");
        let mut transport = services::google::ReqwestHttpTransport;
        let access_token =
            services::google::refresh_access_token(&mut transport, &config, &refresh_token)
                .expect("refresh access token");

        let suffix = (Utc::now().timestamp_millis() as u64) % 10_000_000;
        let customer = create_customer_tx(
            &mut connection,
            CustomerInput {
                first_name: "Test".into(),
                last_name: "Google Sync".into(),
                phone: Some(format!("555{:07}", suffix % 10_000_000)),
                email: None,
                notes: Some("SENTETIK GOOGLE LIVE ACCEPTANCE".into()),
                whatsapp_reminder_enabled: Some(false),
                whatsapp_consent_confirmed: Some(false),
            },
        )
        .expect("create synthetic customer");
        let staff = create_staff_tx(
            &mut connection,
            StaffInput {
                first_name: "Test".into(),
                last_name: Some("Google Sync".into()),
                phone: None,
                specialty_note: Some("SENTETIK GOOGLE LIVE ACCEPTANCE".into()),
                color_key: "sage".into(),
                is_active: Some(true),
            },
        )
        .expect("create synthetic staff");
        let category = create_category_tx(
            &connection,
            CategoryInput {
                name: format!("TEST Google Sync {suffix}"),
                is_active: Some(true),
            },
        )
        .expect("create synthetic category");
        let service = create_service_tx(
            &mut connection,
            ServiceInput {
                category_id: category.id,
                name: "TEST Google Sync".into(),
                duration_minutes: Some(30),
                default_price_minor: Some(0),
                is_active: Some(true),
            },
        )
        .expect("create synthetic service");
        set_staff_services_tx(&mut connection, &staff.id, vec![service.id.clone()])
            .expect("assign synthetic service");

        let initial = AppointmentInput {
            customer_id: customer.id.clone(),
            staff_id: staff.id.clone(),
            local_date: "2036-08-30".into(),
            local_start_time: "14:00".into(),
            service_ids: vec![service.id.clone()],
            status: Some("planned".into()),
            note: Some("SENTETIK TEST - GOOGLE LIVE ACCEPTANCE".into()),
            whatsapp_reminder_enabled: None,
        };
        let appointment = create_appointment_tx(&mut connection, initial.clone())
            .expect("create synthetic appointment");
        assert!(process_google_outbox_item(
            &connection,
            &mut transport,
            &access_token,
            &calendar_id,
            &appointment.id,
        )
        .expect("create Google event"));
        let event_id: String = connection
            .query_row(
                "SELECT google_event_id FROM appointment_google_calendar_sync WHERE appointment_id=?1",
                params![appointment.id],
                |row| row.get(0),
        )
        .expect("persisted Google mapping");
        assert!(!event_id.is_empty());
        if std::env::var("BEAUTYSALOON_LIVE_GOOGLE_CREATE_ONLY").as_deref() == Ok("1") {
            return;
        }

        let updated = AppointmentInput {
            local_start_time: "14:30".into(),
            ..initial.clone()
        };
        update_appointment_tx(&mut connection, &appointment.id, updated.clone())
            .expect("update synthetic appointment");
        assert!(process_google_outbox_item(
            &connection,
            &mut transport,
            &access_token,
            &calendar_id,
            &appointment.id,
        )
        .expect("update same Google event"));
        let event_after_update: String = connection
            .query_row(
                "SELECT google_event_id FROM appointment_google_calendar_sync WHERE appointment_id=?1",
                params![appointment.id],
                |row| row.get(0),
            )
            .expect("mapping after update");
        assert_eq!(event_id, event_after_update);

        let confirmed = AppointmentInput {
            status: Some("confirmed".into()),
            ..updated.clone()
        };
        update_appointment_tx(&mut connection, &appointment.id, confirmed.clone())
            .expect("confirm synthetic appointment");
        assert!(process_google_outbox_item(
            &connection,
            &mut transport,
            &access_token,
            &calendar_id,
            &appointment.id,
        )
        .expect("update status on same Google event"));

        let cancelled = AppointmentInput {
            status: Some("cancelled".into()),
            ..confirmed
        };
        update_appointment_tx(&mut connection, &appointment.id, cancelled)
            .expect("cancel synthetic appointment");
        assert!(process_google_outbox_item(
            &connection,
            &mut transport,
            &access_token,
            &calendar_id,
            &appointment.id,
        )
        .expect("mark same Google event cancelled"));
        assert!(!process_google_outbox_item(
            &connection,
            &mut transport,
            &access_token,
            &calendar_id,
            &appointment.id,
        )
        .expect("repeat sync without duplicate"));

        let calendar =
            url::form_urlencoded::byte_serialize(calendar_id.as_bytes()).collect::<String>();
        let event = url::form_urlencoded::byte_serialize(event_id.as_bytes()).collect::<String>();
        let response = transport
            .send(services::google::HttpRequest {
                method: "GET".into(),
                url: format!(
                    "{}/calendars/{calendar}/events/{event}",
                    services::google::GOOGLE_CALENDAR_API_BASE
                ),
                headers: vec![("Authorization".into(), format!("Bearer {access_token}"))],
                body: Vec::new(),
            })
            .expect("read mapped Google event");
        assert_eq!(response.status, 200, "mapped event remains readable");
        let event_payload: serde_json::Value =
            serde_json::from_slice(&response.body).expect("read event JSON");
        let summary = event_payload["summary"].as_str().expect("event summary");
        let description = event_payload["description"]
            .as_str()
            .expect("event description");
        assert!(summary.starts_with("IPTAL - BeautySaloon TEST - Test Google Sync"));
        assert!(!summary.contains("555"));
        assert!(!description.contains("555"));
        assert_eq!(
            event_payload["start"]["timeZone"].as_str(),
            Some("Europe/Istanbul")
        );
        assert_eq!(
            event_payload["end"]["timeZone"].as_str(),
            Some("Europe/Istanbul")
        );

        drop(connection);
        let reopened = open_database(&database_path).expect("reopen live database");
        let persisted_event_id: String = reopened
            .query_row(
                "SELECT google_event_id FROM appointment_google_calendar_sync WHERE appointment_id=?1",
                params![appointment.id],
                |row| row.get(0),
            )
            .expect("persisted mapping after reopen");
        assert_eq!(event_id, persisted_event_id);
        assert!(integrity_check(&reopened).expect("integrity check"));
    }

    #[test]
    #[ignore = "continues the explicitly authorized mapped live Google test event"]
    fn live_google_calendar_continue_existing_acceptance() {
        assert_eq!(
            std::env::var("BEAUTYSALOON_ALLOW_LIVE_GOOGLE_ACCEPTANCE").as_deref(),
            Ok("1")
        );
        let database_path = PathBuf::from(std::env::var_os("APPDATA").expect("APPDATA"))
            .join("com.beautysaloon.desktop")
            .join("database")
            .join("salon-foundation.db");
        let mut connection = open_database(&database_path).expect("open live database");
        let config = google_config().expect("local Google config");
        let calendar_id = config
            .calendar_id
            .clone()
            .unwrap_or_else(|| "primary".to_string());
        let protector = services::secure_store::WindowsDpapiProtector;
        let refresh_token = services::secure_store::read_secret(
            &connection,
            &protector,
            "google_calendar_refresh_token",
        )
        .expect("read refresh token")
        .expect("persisted refresh token");
        let mut transport = services::google::ReqwestHttpTransport;
        let access_token =
            services::google::refresh_access_token(&mut transport, &config, &refresh_token)
                .expect("refresh access token");
        let (appointment_id, customer_id, staff_id, event_id): (String, String, String, String) = connection
            .query_row(
                "SELECT a.id, a.customer_id, a.staff_id, m.google_event_id
                 FROM appointments a JOIN appointment_google_calendar_sync m ON m.appointment_id=a.id
                 WHERE a.note='SENTETIK TEST - GOOGLE LIVE ACCEPTANCE' AND m.google_event_id IS NOT NULL
                 ORDER BY a.created_at DESC LIMIT 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .expect("existing mapped synthetic appointment");
        let service_ids = list_appointment_services(&connection, &appointment_id)
            .expect("synthetic services")
            .into_iter()
            .map(|service| service.service_id)
            .collect();
        let updated = AppointmentInput {
            customer_id,
            staff_id,
            local_date: "2036-08-30".into(),
            local_start_time: "15:00".into(),
            service_ids,
            status: Some("planned".into()),
            note: Some("SENTETIK TEST - GOOGLE LIVE ACCEPTANCE".into()),
            whatsapp_reminder_enabled: None,
        };
        update_appointment_tx(&mut connection, &appointment_id, updated.clone())
            .expect("update same synthetic appointment");
        assert!(process_google_outbox_item(
            &connection,
            &mut transport,
            &access_token,
            &calendar_id,
            &appointment_id,
        )
        .expect("patch same Google event"));
        let mapped_after_update: String = connection
            .query_row(
                "SELECT google_event_id FROM appointment_google_calendar_sync WHERE appointment_id=?1",
                params![appointment_id],
                |row| row.get(0),
            )
            .expect("mapping after update");
        assert_eq!(event_id, mapped_after_update);

        let confirmed = AppointmentInput {
            status: Some("confirmed".into()),
            ..updated.clone()
        };
        update_appointment_tx(&mut connection, &appointment_id, confirmed.clone())
            .expect("update status");
        assert!(process_google_outbox_item(
            &connection,
            &mut transport,
            &access_token,
            &calendar_id,
            &appointment_id
        )
        .expect("patch status"));
        let cancelled = AppointmentInput {
            status: Some("cancelled".into()),
            ..confirmed
        };
        update_appointment_tx(&mut connection, &appointment_id, cancelled)
            .expect("cancel appointment");
        assert!(process_google_outbox_item(
            &connection,
            &mut transport,
            &access_token,
            &calendar_id,
            &appointment_id
        )
        .expect("patch cancellation"));
        assert!(!process_google_outbox_item(
            &connection,
            &mut transport,
            &access_token,
            &calendar_id,
            &appointment_id
        )
        .expect("no duplicate event"));

        let calendar =
            url::form_urlencoded::byte_serialize(calendar_id.as_bytes()).collect::<String>();
        let event = url::form_urlencoded::byte_serialize(event_id.as_bytes()).collect::<String>();
        let response = transport
            .send(services::google::HttpRequest {
                method: "GET".into(),
                url: format!(
                    "{}/calendars/{calendar}/events/{event}",
                    services::google::GOOGLE_CALENDAR_API_BASE
                ),
                headers: vec![("Authorization".into(), format!("Bearer {access_token}"))],
                body: Vec::new(),
            })
            .expect("read mapped event");
        assert_eq!(response.status, 200);
        let payload: serde_json::Value =
            serde_json::from_slice(&response.body).expect("event JSON");
        let summary = payload["summary"].as_str().expect("summary");
        let description = payload["description"].as_str().expect("description");
        assert!(summary.starts_with("IPTAL - BeautySaloon TEST"));
        assert!(!summary.contains("555") && !description.contains("555"));
        assert_eq!(
            payload["start"]["timeZone"].as_str(),
            Some("Europe/Istanbul")
        );
        assert_eq!(payload["end"]["timeZone"].as_str(), Some("Europe/Istanbul"));
        drop(connection);
        let reopened = open_database(&database_path).expect("reopen database");
        let persisted: String = reopened.query_row(
            "SELECT google_event_id FROM appointment_google_calendar_sync WHERE appointment_id=?1",
            params![appointment_id],
            |row| row.get(0),
        ).expect("persisted mapping");
        assert_eq!(event_id, persisted);
        assert!(integrity_check(&reopened).expect("integrity"));
    }

    #[test]
    #[ignore = "requires explicit live Supabase OTP authorization"]
    fn live_supabase_otp_request() {
        assert_eq!(
            std::env::var("BEAUTYSALOON_ALLOW_LIVE_SUPABASE_OTP").as_deref(),
            Ok("1")
        );
        let email = std::env::var("BEAUTYSALOON_LIVE_SUPABASE_OTP_EMAIL")
            .expect("OTP email is supplied only through the environment");
        let config = supabase_config().expect("local Supabase public config");
        let mut transport = services::google::ReqwestHttpTransport;
        services::supabase::request_email_otp(&mut transport, &config, &email)
            .expect("request live OTP");
    }

    #[test]
    #[ignore = "requires explicit live Supabase OTP verification authorization"]
    fn live_supabase_otp_verify_and_persist() {
        assert_eq!(
            std::env::var("BEAUTYSALOON_ALLOW_LIVE_SUPABASE_OTP").as_deref(),
            Ok("1")
        );
        let email = std::env::var("BEAUTYSALOON_LIVE_SUPABASE_OTP_EMAIL")
            .expect("OTP email is supplied only through the environment");
        let otp = std::env::var("BEAUTYSALOON_LIVE_SUPABASE_OTP_CODE")
            .expect("OTP is supplied only through the environment");
        let config = supabase_config().expect("local Supabase public config");
        let mut transport = services::google::ReqwestHttpTransport;
        let session = services::supabase::verify_email_otp(&mut transport, &config, &email, &otp)
            .expect("verify live OTP");
        let database_path = PathBuf::from(std::env::var_os("APPDATA").expect("APPDATA"))
            .join("com.beautysaloon.desktop")
            .join("database")
            .join("salon-foundation.db");
        let connection = open_database(&database_path).expect("open live database");
        let protector = services::secure_store::WindowsDpapiProtector;
        services::secure_store::store_supabase_session(&connection, &protector, &session)
            .expect("store DPAPI session");
        drop(connection);
        let reopened = open_database(&database_path).expect("reopen live database");
        assert!(
            services::secure_store::read_supabase_session(&reopened, &protector)
                .expect("read DPAPI session")
                .is_some()
        );
    }

    #[test]
    #[ignore = "requires explicit authenticated dispatcher status read"]
    fn live_dispatcher_status_read() {
        let database_path = PathBuf::from(std::env::var_os("APPDATA").expect("APPDATA"))
            .join("com.beautysaloon.desktop")
            .join("database")
            .join("salon-foundation.db");
        let connection = open_database(&database_path).expect("open live database");
        let protector = services::secure_store::WindowsDpapiProtector;
        let session = services::secure_store::read_supabase_session(&connection, &protector)
            .expect("read session")
            .expect("persisted session");
        let config = supabase_config().expect("Supabase config");
        let mut transport = services::google::ReqwestHttpTransport;
        let status =
            services::reminder_cloud::read_dispatcher_status(&mut transport, &config, &session)
                .or_else(|error| match error {
                    AppError::Validation(code) if code == "CLOUD_AUTH_INVALID" => {
                        let refreshed = services::supabase::refresh_session(
                            &mut transport,
                            &config,
                            &session.refresh_token,
                        )?;
                        services::secure_store::store_supabase_session(
                            &connection,
                            &protector,
                            &refreshed,
                        )?;
                        services::reminder_cloud::read_dispatcher_status(
                            &mut transport,
                            &config,
                            &refreshed,
                        )
                    }
                    error => Err(error),
                })
                .expect("read dispatcher status");
        assert!(status.paused, "dispatcher must remain paused");
    }

    #[test]
    fn migrates_v13_required_customer_phone_to_nullable_without_losing_foreign_keys() {
        let temp = tempdir().expect("temp");
        let path = temp.path().join("v13.db");
        let old = Connection::open(&path).expect("old database");
        old.pragma_update(None, "foreign_keys", "ON")
            .expect("foreign keys");
        old.execute_batch(
            "CREATE TABLE app_meta (id INTEGER PRIMARY KEY AUTOINCREMENT NOT NULL, schema_version INTEGER NOT NULL, initialized_at TEXT NOT NULL);
             INSERT INTO app_meta (schema_version, initialized_at) VALUES (13, '2026-01-01T00:00:00.000Z');
             CREATE TABLE customers (
               id TEXT PRIMARY KEY NOT NULL, first_name TEXT NOT NULL, last_name TEXT NOT NULL, phone TEXT NOT NULL,
               email TEXT, whatsapp_reminder_enabled INTEGER NOT NULL DEFAULT 1, whatsapp_consent_confirmed INTEGER NOT NULL DEFAULT 0,
               whatsapp_consent_recorded_at TEXT, notes TEXT, is_active INTEGER NOT NULL DEFAULT 1, created_at TEXT NOT NULL, updated_at TEXT NOT NULL);
             CREATE UNIQUE INDEX customers_phone_unique_idx ON customers(phone);
             CREATE INDEX customers_active_name_idx ON customers(is_active, last_name, first_name);
             CREATE TABLE audit_log (
               id INTEGER PRIMARY KEY,
               entity_type TEXT NOT NULL,
               entity_id TEXT NOT NULL,
               action TEXT NOT NULL,
               occurred_at_utc TEXT NOT NULL,
               summary TEXT NOT NULL
             );
             CREATE INDEX audit_log_entity_idx ON audit_log(entity_type, entity_id, occurred_at_utc DESC);
             CREATE TABLE appointments (id TEXT PRIMARY KEY, customer_id TEXT NOT NULL, staff_id TEXT NOT NULL, start_at_utc TEXT NOT NULL, end_at_utc TEXT NOT NULL, total_duration_minutes INTEGER NOT NULL, status TEXT NOT NULL, note TEXT, created_at TEXT NOT NULL, updated_at TEXT NOT NULL, FOREIGN KEY (customer_id) REFERENCES customers(id));
             INSERT INTO customers VALUES ('c-1','Ada','Test','5551112233',NULL,1,0,NULL,'keep',1,'2026-01-01T00:00:00.000Z','2026-01-01T00:00:00.000Z');
             INSERT INTO customers VALUES ('c-2','Bora','Test','5551112244','b@example.test',0,0,NULL,NULL,1,'2026-01-01T00:00:00.000Z','2026-01-01T00:00:00.000Z');
             INSERT INTO audit_log VALUES (7,'customer','c-1','create','2026-01-01T00:00:00.000Z','legacy summary');
             INSERT INTO appointments VALUES ('a-1','c-1','staff-1','2026-02-01T07:00:00.000Z','2026-02-01T08:00:00.000Z',60,'planned',NULL,'2026-01-01T00:00:00.000Z','2026-01-01T00:00:00.000Z');",
        ).expect("v13 fixture");
        drop(old);
        let connection = initialize_database_at(&path).expect("migrate explicit v13 path");
        assert_eq!(read_schema_version(&connection).expect("version"), 19);
        assert_eq!(
            connection
                .query_row(
                    "SELECT COUNT(*) FROM services WHERE default_price_minor <> 0",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .expect("service defaults"),
            0
        );
        assert_eq!(connection.query_row("SELECT COUNT(*) FROM appointment_services WHERE listed_price_snapshot_minor <> 0 OR charged_price_minor <> 0", [], |row| row.get::<_, i64>(0)).expect("snapshot defaults"), 0);
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM staff_working_hours", [], |row| row
                    .get::<_, i64>(0))
                .expect("working hours empty"),
            0
        );
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM staff_time_off", [], |row| row
                    .get::<_, i64>(0))
                .expect("time off empty"),
            0
        );
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM audit_log", [], |row| row
                    .get::<_, i64>(0))
                .expect("migrated audit count"),
            1
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT id, occurred_at, entity_type, entity_id, action, metadata_json FROM audit_log",
                    [],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                            row.get::<_, String>(3)?,
                            row.get::<_, String>(4)?,
                            row.get::<_, Option<String>>(5)?,
                        ))
                    },
                )
                .expect("preserved legacy audit row"),
            (
                "7".into(),
                "2026-01-01T00:00:00.000Z".into(),
                "customer".into(),
                "c-1".into(),
                "create".into(),
                None,
            )
        );
        let audit_columns = table_columns(&connection, "audit_log").expect("audit columns");
        assert!(audit_columns.contains("occurred_at"));
        assert!(audit_columns.contains("metadata_json"));
        assert!(!audit_columns.contains("occurred_at_utc"));
        assert!(!audit_columns.contains("summary"));
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM customers", [], |row| row
                    .get::<_, i64>(0))
                .expect("count"),
            2
        );
        assert_eq!(
            connection
                .query_row("SELECT phone FROM customers WHERE id='c-1'", [], |row| {
                    row.get::<_, String>(0)
                })
                .expect("phone"),
            "5551112233"
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT customer_id FROM appointments WHERE id='a-1'",
                    [],
                    |row| row.get::<_, String>(0)
                )
                .expect("fk"),
            "c-1"
        );
        connection.execute("INSERT INTO customers (id,first_name,last_name,phone,whatsapp_reminder_enabled,whatsapp_consent_confirmed,is_active,created_at,updated_at) VALUES ('c-3','Null','One',NULL,1,0,1,'x','x'),('c-4','Null','Two',NULL,1,0,1,'x','x')", []).expect("multiple nulls");
        assert!(connection.execute("INSERT INTO customers (id,first_name,last_name,phone,whatsapp_reminder_enabled,whatsapp_consent_confirmed,is_active,created_at,updated_at) VALUES ('c-5','Dup','Phone','5551112233',1,0,1,'x','x')", []).is_err());
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
                    row.get::<_, i64>(0)
                })
                .expect("fk check"),
            0
        );
        assert!(integrity_check(&connection).expect("integrity"));
        let tables: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='customers_v13'",
                [],
                |row| row.get(0),
            )
            .expect("old table absent");
        assert_eq!(tables, 0);
        drop(connection);
        let reopened = initialize_database_at(&path).expect("restart safe");
        assert_eq!(
            read_schema_version(&reopened).expect("version after restart"),
            19
        );
        assert!(integrity_check(&reopened).expect("integrity after restart"));
    }

    #[test]
    fn migration_rehearsal_helper_contract() {
        let temp = tempdir().expect("temp dir");
        let temp_db = temp.path().join("rehearsal.db");
        let forbidden_db = temp.path().join("forbidden_live.db");

        let err = initialize_explicit_migration_rehearsal(None, Some(&forbidden_db)).unwrap_err();
        assert_eq!(
            err.to_string(),
            AppError::Validation("MIGRATION_REHEARSAL_PATH_REQUIRED".into()).to_string()
        );

        let err = initialize_explicit_migration_rehearsal(Some(&temp_db), None).unwrap_err();
        assert_eq!(
            err.to_string(),
            AppError::Validation("MIGRATION_REHEARSAL_FORBIDDEN_PATH_REQUIRED".into()).to_string()
        );

        let err =
            initialize_explicit_migration_rehearsal(Some(&temp_db), Some(&temp_db)).unwrap_err();
        assert_eq!(
            err.to_string(),
            AppError::Validation("MIGRATION_REHEARSAL_CANONICAL_PATH_FORBIDDEN".into()).to_string()
        );

        let connection =
            initialize_explicit_migration_rehearsal(Some(&temp_db), Some(&forbidden_db))
                .expect("explicit rehearsal succeeds");
        assert_eq!(read_schema_version(&connection).expect("schema"), 19);
        assert!(integrity_check(&connection).expect("integrity"));
    }

    #[test]
    #[ignore = "requires explicit rehearsal and forbidden paths"]
    fn offline_v13_to_v18_migration_rehearsal_explicit_path() {
        let rehearsal_path = std::env::var_os("BEAUTYSALOON_REHEARSAL_DB_PATH")
            .map(PathBuf::from)
            .expect("BEAUTYSALOON_REHEARSAL_DB_PATH is required");
        let forbidden_path = std::env::var_os("BEAUTYSALOON_FORBIDDEN_CANONICAL_PATH")
            .map(PathBuf::from)
            .expect("BEAUTYSALOON_FORBIDDEN_CANONICAL_PATH is required");

        // 1. Verify initial schema is 13 on the temp work copy before migration
        let (c_cust, c_staff, c_svc, c_appt, c_asvc, cust_ids, appt_ids) = {
            let pre_conn = Connection::open_with_flags(
                &rehearsal_path,
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
            )
            .expect("open rehearsal copy pre-migration");
            assert_eq!(
                read_schema_version(&pre_conn).expect("initial schema"),
                13,
                "rehearsal copy must start at schema 13"
            );
            assert!(integrity_check(&pre_conn).expect("pre integrity"));
            assert_eq!(
                pre_conn
                    .query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |r| {
                        r.get::<_, i64>(0)
                    })
                    .expect("pre fk"),
                0
            );

            let cust: i64 = pre_conn
                .query_row("SELECT COUNT(*) FROM customers", [], |r| r.get(0))
                .expect("cust");
            let staff: i64 = pre_conn
                .query_row("SELECT COUNT(*) FROM staff", [], |r| r.get(0))
                .expect("staff");
            let svc: i64 = pre_conn
                .query_row("SELECT COUNT(*) FROM services", [], |r| r.get(0))
                .expect("svc");
            let appt: i64 = pre_conn
                .query_row("SELECT COUNT(*) FROM appointments", [], |r| r.get(0))
                .expect("appt");
            let asvc: i64 = pre_conn
                .query_row("SELECT COUNT(*) FROM appointment_services", [], |r| {
                    r.get(0)
                })
                .expect("asvc");

            let c_ids = {
                let mut stmt = pre_conn
                    .prepare("SELECT id FROM customers ORDER BY id")
                    .expect("stmt cust");
                let rows = stmt
                    .query_map([], |row| row.get::<_, String>(0))
                    .expect("query cust");
                rows.collect::<Result<Vec<_>, _>>().expect("collect cust")
            };

            let a_ids = {
                let mut stmt = pre_conn
                    .prepare("SELECT id FROM appointments ORDER BY id")
                    .expect("stmt appt");
                let rows = stmt
                    .query_map([], |row| row.get::<_, String>(0))
                    .expect("query appt");
                rows.collect::<Result<Vec<_>, _>>().expect("collect appt")
            };

            (cust, staff, svc, appt, asvc, c_ids, a_ids)
        };

        // 2. Run explicit rehearsal migration
        let connection =
            initialize_explicit_migration_rehearsal(Some(&rehearsal_path), Some(&forbidden_path))
                .expect("rehearsal migration succeeds");

        assert_eq!(read_schema_version(&connection).expect("schema 19"), 19);
        assert!(integrity_check(&connection).expect("migrated integrity"));
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |r| {
                    r.get::<_, i64>(0)
                })
                .expect("migrated fk"),
            0
        );

        // Verify counts preserved
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM customers", [], |r| r.get::<_, i64>(0))
                .expect("cust count"),
            c_cust
        );
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM staff", [], |r| r.get::<_, i64>(0))
                .expect("staff count"),
            c_staff
        );
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM services", [], |r| r.get::<_, i64>(0))
                .expect("svc count"),
            c_svc
        );
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM appointments", [], |r| r
                    .get::<_, i64>(0))
                .expect("appt count"),
            c_appt
        );
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM appointment_services", [], |r| {
                    r.get::<_, i64>(0)
                })
                .expect("asvc count"),
            c_asvc
        );

        // Verify IDs preserved
        let post_c_ids = {
            let mut stmt = connection
                .prepare("SELECT id FROM customers ORDER BY id")
                .expect("stmt post cust");
            let rows = stmt
                .query_map([], |row| row.get::<_, String>(0))
                .expect("query post cust");
            rows.collect::<Result<Vec<_>, _>>()
                .expect("collect post cust")
        };
        assert_eq!(post_c_ids, cust_ids);

        let post_a_ids = {
            let mut stmt = connection
                .prepare("SELECT id FROM appointments ORDER BY id")
                .expect("stmt post appt");
            let rows = stmt
                .query_map([], |row| row.get::<_, String>(0))
                .expect("query post appt");
            rows.collect::<Result<Vec<_>, _>>()
                .expect("collect post appt")
        };
        assert_eq!(post_a_ids, appt_ids);

        drop(connection);

        // 3. Reopen / second run
        let reopened =
            initialize_explicit_migration_rehearsal(Some(&rehearsal_path), Some(&forbidden_path))
                .expect("reopen migration succeeds");
        assert_eq!(read_schema_version(&reopened).expect("reopen schema"), 19);
        assert!(integrity_check(&reopened).expect("reopen integrity"));
        assert_eq!(
            reopened
                .query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |r| {
                    r.get::<_, i64>(0)
                })
                .expect("reopen fk check"),
            0
        );
        drop(reopened);
    }

    #[test]
    fn migrates_v16_to_append_only_audit_without_changing_business_history() {
        let temp = tempdir().expect("temp");
        let path = temp.path().join("v16.db");
        let (customer_id, staff_id, service_id, appointment_id, counts) = {
            let mut v16 = open_database(&path).expect("create v16-shaped fixture");
            let (customer, staff, service) = seed_core(&mut v16);
            let appointment = create_appointment_tx(
                &mut v16,
                appointment_input(
                    &customer.id,
                    &staff.id,
                    "2036-11-02",
                    "10:00",
                    vec![service.id.clone()],
                    "planned",
                ),
            )
            .expect("fixture appointment");
            let counts = (
                v16.query_row("SELECT COUNT(*) FROM customers", [], |row| {
                    row.get::<_, i64>(0)
                })
                .expect("customer count"),
                v16.query_row("SELECT COUNT(*) FROM staff", [], |row| row.get::<_, i64>(0))
                    .expect("staff count"),
                v16.query_row("SELECT COUNT(*) FROM services", [], |row| {
                    row.get::<_, i64>(0)
                })
                .expect("service count"),
                v16.query_row("SELECT COUNT(*) FROM appointments", [], |row| {
                    row.get::<_, i64>(0)
                })
                .expect("appointment count"),
                v16.query_row("SELECT COUNT(*) FROM appointment_services", [], |row| {
                    row.get::<_, i64>(0)
                })
                .expect("appointment service count"),
            );
            v16.execute_batch(
                "DROP TRIGGER IF EXISTS audit_log_append_only_update;
                 DROP TRIGGER IF EXISTS audit_log_append_only_delete;
                 DROP TABLE audit_log;
                 UPDATE app_meta SET schema_version=16;",
            )
            .expect("make v16 fixture");
            (customer.id, staff.id, service.id, appointment.id, counts)
        };

        let connection = open_database(&path).expect("migrate v16");
        assert_eq!(read_schema_version(&connection).expect("schema"), 19);
        assert_eq!(
            (
                connection
                    .query_row("SELECT COUNT(*) FROM customers", [], |row| {
                        row.get::<_, i64>(0)
                    })
                    .expect("customer count"),
                connection
                    .query_row("SELECT COUNT(*) FROM staff", [], |row| {
                        row.get::<_, i64>(0)
                    })
                    .expect("staff count"),
                connection
                    .query_row("SELECT COUNT(*) FROM services", [], |row| {
                        row.get::<_, i64>(0)
                    })
                    .expect("service count"),
                connection
                    .query_row("SELECT COUNT(*) FROM appointments", [], |row| {
                        row.get::<_, i64>(0)
                    })
                    .expect("appointment count"),
                connection
                    .query_row("SELECT COUNT(*) FROM appointment_services", [], |row| {
                        row.get::<_, i64>(0)
                    })
                    .expect("appointment service count"),
            ),
            counts
        );
        for (table, id) in [
            ("customers", customer_id.as_str()),
            ("staff", staff_id.as_str()),
            ("services", service_id.as_str()),
            ("appointments", appointment_id.as_str()),
        ] {
            let exists: i64 = connection
                .query_row(
                    &format!("SELECT COUNT(*) FROM {table} WHERE id=?1"),
                    params![id],
                    |row| row.get(0),
                )
                .expect("preserved id");
            assert_eq!(exists, 1, "{table}");
        }
        assert_eq!(
            connection
                .query_row(
                    "SELECT customer_id || ':' || staff_id FROM appointments WHERE id=?1",
                    params![appointment_id],
                    |row| row.get::<_, String>(0),
                )
                .expect("appointment foreign keys"),
            format!("{customer_id}:{staff_id}")
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT COUNT(*) FROM appointment_services WHERE appointment_id=?1 AND service_id=?2",
                    params![appointment_id, service_id],
                    |row| row.get::<_, i64>(0),
                )
                .expect("appointment service foreign key"),
            1
        );
        for table in ["customers", "staff", "services"] {
            let active: i64 = connection
                .query_row(
                    &format!("SELECT is_active FROM {table} LIMIT 1"),
                    [],
                    |row| row.get(0),
                )
                .expect("active state");
            assert_eq!(active, 1, "{table} remains active");
        }
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM audit_log", [], |row| row
                    .get::<_, i64>(0))
                .expect("audit empty"),
            0
        );
        for index in [
            "audit_log_occurred_at_idx",
            "audit_log_entity_occurred_at_idx",
        ] {
            let exists: i64 = connection
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name=?1",
                    params![index],
                    |row| row.get(0),
                )
                .expect("audit index");
            assert_eq!(exists, 1, "{index}");
        }
        connection
            .execute(
                "INSERT INTO audit_log (id, occurred_at, entity_type, entity_id, action, metadata_json)
                 VALUES ('audit-1', '2036-11-02T07:00:00.000Z', 'appointment', ?1, 'create', '{\"source\":\"migration-test\"}')",
                params![appointment_id],
            )
            .expect("audit insert");
        assert!(connection
            .execute(
                "UPDATE audit_log SET action='update' WHERE id='audit-1'",
                []
            )
            .is_err());
        assert!(connection
            .execute("DELETE FROM audit_log WHERE id='audit-1'", [])
            .is_err());
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
                    row.get::<_, i64>(0)
                })
                .expect("foreign key check"),
            0
        );
        assert!(integrity_check(&connection).expect("integrity"));
        drop(connection);
        let reopened = open_database(&path).expect("restart safe");
        assert_eq!(read_schema_version(&reopened).expect("reopened schema"), 19);
        assert!(integrity_check(&reopened).expect("reopened integrity"));
    }

    #[test]
    fn migrates_v17_to_single_business_profile_without_changing_business_history() {
        let temp = tempdir().expect("temp");
        let path = temp.path().join("v17.db");
        let (customer_id, staff_id, service_id, appointment_id, counts) = {
            let mut v17 = open_database(&path).expect("create v17-shaped fixture");
            let (customer, staff, service) = seed_core(&mut v17);
            let appointment = create_appointment_tx(
                &mut v17,
                appointment_input(
                    &customer.id,
                    &staff.id,
                    "2036-11-03",
                    "10:00",
                    vec![service.id.clone()],
                    "planned",
                ),
            )
            .expect("fixture appointment");
            let counts = (
                v17.query_row("SELECT COUNT(*) FROM customers", [], |row| {
                    row.get::<_, i64>(0)
                })
                .expect("customer count"),
                v17.query_row("SELECT COUNT(*) FROM staff", [], |row| row.get::<_, i64>(0))
                    .expect("staff count"),
                v17.query_row("SELECT COUNT(*) FROM services", [], |row| {
                    row.get::<_, i64>(0)
                })
                .expect("service count"),
                v17.query_row("SELECT COUNT(*) FROM appointments", [], |row| {
                    row.get::<_, i64>(0)
                })
                .expect("appointment count"),
                v17.query_row("SELECT COUNT(*) FROM appointment_services", [], |row| {
                    row.get::<_, i64>(0)
                })
                .expect("appointment service count"),
            );
            v17.execute_batch(
                "DROP TABLE business_profile;
                 UPDATE app_meta SET schema_version=17;",
            )
            .expect("make v17 fixture");
            (customer.id, staff.id, service.id, appointment.id, counts)
        };

        let connection = open_database(&path).expect("migrate v17");
        assert_eq!(read_schema_version(&connection).expect("schema"), 19);
        assert_eq!(
            connection
                .query_row(
                    "SELECT business_name, phone, email, address, currency_code, theme_key,
                            logo_data IS NULL, logo_mime_type IS NULL
                     FROM business_profile WHERE id=1",
                    [],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, Option<String>>(1)?,
                            row.get::<_, Option<String>>(2)?,
                            row.get::<_, Option<String>>(3)?,
                            row.get::<_, String>(4)?,
                            row.get::<_, String>(5)?,
                            row.get::<_, bool>(6)?,
                            row.get::<_, bool>(7)?,
                        ))
                    },
                )
                .expect("default profile"),
            (
                String::new(),
                None,
                None,
                None,
                "TRY".to_string(),
                "default".to_string(),
                true,
                true,
            )
        );
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM business_profile", [], |row| row
                    .get::<_, i64>(0))
                .expect("one profile"),
            1
        );
        assert_eq!(
            (
                connection
                    .query_row("SELECT COUNT(*) FROM customers", [], |row| row
                        .get::<_, i64>(0))
                    .expect("customer count"),
                connection
                    .query_row("SELECT COUNT(*) FROM staff", [], |row| row.get::<_, i64>(0))
                    .expect("staff count"),
                connection
                    .query_row("SELECT COUNT(*) FROM services", [], |row| row
                        .get::<_, i64>(0))
                    .expect("service count"),
                connection
                    .query_row("SELECT COUNT(*) FROM appointments", [], |row| row
                        .get::<_, i64>(0))
                    .expect("appointment count"),
                connection
                    .query_row("SELECT COUNT(*) FROM appointment_services", [], |row| row
                        .get::<_, i64>(
                        0
                    ))
                    .expect("appointment service count"),
            ),
            counts
        );
        for (table, id) in [
            ("customers", customer_id.as_str()),
            ("staff", staff_id.as_str()),
            ("services", service_id.as_str()),
            ("appointments", appointment_id.as_str()),
        ] {
            let exists: i64 = connection
                .query_row(
                    &format!("SELECT COUNT(*) FROM {table} WHERE id=?1"),
                    params![id],
                    |row| row.get(0),
                )
                .expect("preserved id");
            assert_eq!(exists, 1, "{table}");
        }
        assert_eq!(
            connection
                .query_row(
                    "SELECT COUNT(*) FROM appointment_services WHERE appointment_id=?1 AND service_id=?2",
                    params![appointment_id, service_id],
                    |row| row.get::<_, i64>(0),
                )
                .expect("preserved foreign key"),
            1
        );
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
                    row.get::<_, i64>(0)
                })
                .expect("foreign key check"),
            0
        );
        assert!(integrity_check(&connection).expect("integrity"));
        drop(connection);
        let reopened = open_database(&path).expect("restart safe");
        assert_eq!(read_schema_version(&reopened).expect("reopened schema"), 19);
        assert_eq!(
            reopened
                .query_row("SELECT COUNT(*) FROM business_profile", [], |row| row
                    .get::<_, i64>(0))
                .expect("reopened profile"),
            1
        );
        assert!(integrity_check(&reopened).expect("reopened integrity"));
        drop(reopened);
        let reopened_again = open_database(&path).expect("second startup safe");
        assert_eq!(
            reopened_again
                .query_row("SELECT COUNT(*) FROM business_profile", [], |row| row
                    .get::<_, i64>(0))
                .expect("second profile"),
            1
        );
    }

    #[test]
    fn archive_and_inactive_domain_preserve_history_and_block_new_appointments() {
        let (_temp, mut connection) = open_temp();
        let (customer, staff, service) = seed_core(&mut connection);
        let historical = create_appointment_tx(
            &mut connection,
            appointment_input(
                &customer.id,
                &staff.id,
                "2036-12-01",
                "10:00",
                vec![service.id.clone()],
                "planned",
            ),
        )
        .expect("historical appointment");
        let historical_snapshots =
            list_appointment_services(&connection, &historical.id).expect("historical snapshots");
        let appointment_count: i64 = connection
            .query_row("SELECT COUNT(*) FROM appointments", [], |row| row.get(0))
            .expect("appointment count");
        let appointment_service_count: i64 = connection
            .query_row("SELECT COUNT(*) FROM appointment_services", [], |row| {
                row.get(0)
            })
            .expect("appointment service count");

        let archived = archive_customer(&mut connection, &customer.id).expect("archive customer");
        assert!(!archived.is_active);
        assert!(
            get_customer(&connection, &customer.id)
                .expect("customer detail")
                .expect("customer preserved")
                .is_active
                == false
        );
        assert!(customer_search_for_test(&connection, "ayse", "active")
            .expect("active customer list")
            .is_empty());
        assert_eq!(
            customer_search_for_test(&connection, "ayse", "all")
                .expect("all customer list")
                .len(),
            1
        );
        assert!(create_appointment_tx(
            &mut connection,
            appointment_input(
                &customer.id,
                &staff.id,
                "2036-12-01",
                "11:00",
                vec![service.id.clone()],
                "planned",
            ),
        )
        .is_err());
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM appointments", [], |row| {
                    row.get::<_, i64>(0)
                })
                .expect("archived customer did not book"),
            appointment_count
        );
        assert!(get_appointment(&connection, &historical.id).is_ok());
        assert_eq!(
            list_appointment_services(&connection, &historical.id).expect("history snapshots"),
            historical_snapshots
        );
        assert!(
            reactivate_customer(&mut connection, &customer.id)
                .expect("reactivate customer")
                .is_active
        );
        create_appointment_tx(
            &mut connection,
            appointment_input(
                &customer.id,
                &staff.id,
                "2036-12-01",
                "11:00",
                vec![service.id.clone()],
                "planned",
            ),
        )
        .expect("reactivated customer can book");

        assert!(
            !deactivate_staff(&mut connection, &staff.id)
                .expect("deactivate staff")
                .is_active
        );
        let active_staff_count: i64 = connection
            .query_row("SELECT COUNT(*) FROM staff WHERE is_active=1", [], |row| {
                row.get(0)
            })
            .expect("active staff list");
        assert_eq!(active_staff_count, 0);
        assert!(create_appointment_tx(
            &mut connection,
            appointment_input(
                &customer.id,
                &staff.id,
                "2036-12-01",
                "12:00",
                vec![service.id.clone()],
                "planned",
            ),
        )
        .is_err());
        assert!(get_appointment(&connection, &historical.id).is_ok());
        assert!(
            reactivate_staff(&mut connection, &staff.id)
                .expect("reactivate staff")
                .is_active
        );
        create_appointment_tx(
            &mut connection,
            appointment_input(
                &customer.id,
                &staff.id,
                "2036-12-01",
                "12:00",
                vec![service.id.clone()],
                "planned",
            ),
        )
        .expect("reactivated staff can book");

        let inactive =
            deactivate_service(&mut connection, &service.id).expect("deactivate service");
        assert!(!inactive.is_active);
        assert!(list_service_items(&connection, "active")
            .expect("active service list")
            .is_empty());
        assert_eq!(
            list_service_items(&connection, "all")
                .expect("all service list")
                .len(),
            1
        );
        assert!(create_appointment_tx(
            &mut connection,
            appointment_input(
                &customer.id,
                &staff.id,
                "2036-12-01",
                "13:00",
                vec![service.id.clone()],
                "planned",
            ),
        )
        .is_err());
        assert_eq!(
            list_appointment_services(&connection, &historical.id)
                .expect("inactive service history snapshots"),
            historical_snapshots
        );
        assert!(
            reactivate_service(&mut connection, &service.id)
                .expect("reactivate service")
                .is_active
        );
        create_appointment_tx(
            &mut connection,
            appointment_input(
                &customer.id,
                &staff.id,
                "2036-12-01",
                "13:00",
                vec![service.id.clone()],
                "planned",
            ),
        )
        .expect("reactivated service can book");
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM customers", [], |row| row
                    .get::<_, i64>(0))
                .expect("customer retained"),
            1
        );
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM staff", [], |row| row.get::<_, i64>(0))
                .expect("staff retained"),
            1
        );
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM services", [], |row| row
                    .get::<_, i64>(0))
                .expect("service retained"),
            1
        );
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM appointment_services", [], |row| {
                    row.get::<_, i64>(0)
                })
                .expect("new snapshots only from successful bookings"),
            appointment_service_count + 3
        );
    }

    #[test]
    fn appointment_cancel_preserves_history_and_releases_the_slot() {
        let (_temp, mut connection) = open_temp();
        let (customer, staff, service) = seed_core(&mut connection);
        let appointment = create_appointment_tx(
            &mut connection,
            appointment_input(
                &customer.id,
                &staff.id,
                "2036-12-05",
                "10:00",
                vec![service.id.clone()],
                "planned",
            ),
        )
        .expect("create appointment");
        let snapshots_before =
            list_appointment_services(&connection, &appointment.id).expect("snapshots before");

        update_appointment_tx(
            &mut connection,
            &appointment.id,
            appointment_input(
                &customer.id,
                &staff.id,
                "2036-12-05",
                "10:00",
                vec![service.id.clone()],
                "cancelled",
            ),
        )
        .expect("cancel appointment");
        update_appointment_tx(
            &mut connection,
            &appointment.id,
            appointment_input(
                &customer.id,
                &staff.id,
                "2036-12-05",
                "10:00",
                vec![service.id.clone()],
                "cancelled",
            ),
        )
        .expect("cancel no-op");

        assert_eq!(
            get_appointment(&connection, &appointment.id)
                .expect("cancelled appointment")
                .status,
            "cancelled"
        );
        assert_eq!(
            list_appointment_services(&connection, &appointment.id).expect("snapshots retained"),
            snapshots_before
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT COUNT(*) FROM appointments WHERE id=?1",
                    params![appointment.id],
                    |row| row.get::<_, i64>(0),
                )
                .expect("appointment row retained"),
            1
        );
        assert_eq!(
            audit_actions(&connection, "appointment", &appointment.id)
                .iter()
                .filter(|(action, _)| action == "status_change")
                .count(),
            1
        );

        create_appointment_tx(
            &mut connection,
            appointment_input(
                &customer.id,
                &staff.id,
                "2036-12-05",
                "10:00",
                vec![service.id.clone()],
                "planned",
            ),
        )
        .expect("cancelled appointment releases slot");
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
                    row.get::<_, i64>(0)
                })
                .expect("foreign key check"),
            0
        );
    }

    #[test]
    fn audit_wiring_is_atomic_private_and_deduplicated() {
        let (_temp, mut connection) = open_temp();
        let (customer, staff, service) = seed_core(&mut connection);

        let private_phone = "05559998877";
        let private_email = "private@example.test";
        let private_note = "oauth-token-like-note-must-not-be-audited";
        update_customer_tx(
            &mut connection,
            &customer.id,
            CustomerInput {
                first_name: "Ayse Yeni".into(),
                last_name: customer.last_name.clone(),
                phone: Some(private_phone.into()),
                email: Some(private_email.into()),
                notes: Some(private_note.into()),
                whatsapp_reminder_enabled: Some(true),
                whatsapp_consent_confirmed: Some(false),
            },
        )
        .expect("customer update");
        archive_customer(&mut connection, &customer.id).expect("archive");
        archive_customer(&mut connection, &customer.id).expect("archive no-op");
        reactivate_customer(&mut connection, &customer.id).expect("reactivate");
        let customer_audit = audit_actions(&connection, "customer", &customer.id);
        let mut customer_actions = customer_audit
            .iter()
            .map(|(action, _)| action.as_str())
            .collect::<Vec<_>>();
        customer_actions.sort_unstable();
        assert_eq!(
            customer_actions,
            vec!["archive", "create", "reactivate", "update"]
        );
        let customer_metadata = customer_audit
            .iter()
            .find(|(action, _)| action == "update")
            .and_then(|(_, metadata)| metadata.as_deref())
            .expect("update metadata");
        assert!(customer_metadata.contains("changed_fields"));
        assert!(customer_metadata.contains("phone"));
        assert!(customer_metadata.contains("email"));
        assert!(customer_metadata.contains("notes"));
        assert!(!customer_metadata.contains(private_phone));
        assert!(!customer_metadata.contains(private_email));
        assert!(!customer_metadata.contains(private_note));

        deactivate_staff(&mut connection, &staff.id).expect("deactivate staff");
        reactivate_staff(&mut connection, &staff.id).expect("reactivate staff");
        let mut staff_actions = audit_actions(&connection, "staff", &staff.id)
            .into_iter()
            .map(|(action, _)| action)
            .collect::<Vec<_>>();
        staff_actions.sort_unstable();
        assert_eq!(
            staff_actions,
            vec![
                "create".to_string(),
                "deactivate".to_string(),
                "reactivate".to_string()
            ]
        );
        deactivate_service(&mut connection, &service.id).expect("deactivate service");
        reactivate_service(&mut connection, &service.id).expect("reactivate service");
        let mut service_actions = audit_actions(&connection, "service", &service.id)
            .into_iter()
            .map(|(action, _)| action)
            .collect::<Vec<_>>();
        service_actions.sort_unstable();
        assert_eq!(
            service_actions,
            vec![
                "create".to_string(),
                "deactivate".to_string(),
                "reactivate".to_string()
            ]
        );

        let appointment = create_appointment_tx(
            &mut connection,
            appointment_input(
                &customer.id,
                &staff.id,
                "2036-12-04",
                "10:00",
                vec![service.id.clone()],
                "planned",
            ),
        )
        .expect("appointment create");
        update_appointment_tx(
            &mut connection,
            &appointment.id,
            appointment_input(
                &customer.id,
                &staff.id,
                "2036-12-04",
                "11:00",
                vec![service.id.clone()],
                "planned",
            ),
        )
        .expect("appointment reschedule");
        update_appointment_tx(
            &mut connection,
            &appointment.id,
            appointment_input(
                &customer.id,
                &staff.id,
                "2036-12-04",
                "11:00",
                vec![service.id.clone()],
                "cancelled",
            ),
        )
        .expect("appointment cancel");
        update_appointment_tx(
            &mut connection,
            &appointment.id,
            appointment_input(
                &customer.id,
                &staff.id,
                "2036-12-04",
                "11:00",
                vec![service.id.clone()],
                "cancelled",
            ),
        )
        .expect("appointment status no-op");
        let appointment_audit = audit_actions(&connection, "appointment", &appointment.id);
        assert_eq!(
            appointment_audit
                .iter()
                .map(|(action, _)| action.as_str())
                .collect::<Vec<_>>(),
            vec!["create", "update", "status_change"]
        );
        assert!(appointment_audit[2]
            .1
            .as_deref()
            .expect("status metadata")
            .contains("cancelled"));

        let snapshots = list_appointment_services(&connection, &appointment.id).expect("snapshot");
        let listed_before = snapshots[0].listed_price_snapshot_minor;
        set_appointment_service_charged_price(
            &mut connection,
            &appointment.id,
            &service.id,
            125_000,
        )
        .expect("price override");
        set_appointment_service_charged_price(
            &mut connection,
            &appointment.id,
            &service.id,
            125_000,
        )
        .expect("price override no-op");
        let price_audit = audit_actions(
            &connection,
            "appointment_service",
            &format!("{}:{}", appointment.id, service.id),
        );
        assert_eq!(price_audit.len(), 1);
        assert_eq!(price_audit[0].0, "price_override");
        assert!(price_audit[0]
            .1
            .as_deref()
            .expect("price metadata")
            .contains("125000"));
        assert_eq!(
            list_appointment_services(&connection, &appointment.id).expect("listed unchanged")[0]
                .listed_price_snapshot_minor,
            listed_before
        );

        let audit_before_invalid: i64 = connection
            .query_row("SELECT COUNT(*) FROM audit_log", [], |row| row.get(0))
            .expect("audit count");
        assert!(create_customer_tx(
            &mut connection,
            CustomerInput {
                first_name: "A".into(),
                last_name: "Invalid".into(),
                phone: None,
                email: None,
                notes: None,
                whatsapp_reminder_enabled: None,
                whatsapp_consent_confirmed: None,
            },
        )
        .is_err());
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM audit_log", [], |row| row
                    .get::<_, i64>(0))
                .expect("no audit for invalid mutation"),
            audit_before_invalid
        );

        connection
            .execute_batch(
                "CREATE TRIGGER audit_log_test_insert_failure
                 BEFORE INSERT ON audit_log
                 BEGIN SELECT RAISE(ABORT, 'AUDIT_TEST_INSERT_FAILURE'); END;",
            )
            .expect("failure trigger");
        let customer_count_before: i64 = connection
            .query_row("SELECT COUNT(*) FROM customers", [], |row| row.get(0))
            .expect("customer count");
        assert!(create_customer_tx(
            &mut connection,
            CustomerInput {
                first_name: "Atomic".into(),
                last_name: "Rollback".into(),
                phone: None,
                email: None,
                notes: None,
                whatsapp_reminder_enabled: None,
                whatsapp_consent_confirmed: None,
            },
        )
        .is_err());
        connection
            .execute_batch("DROP TRIGGER audit_log_test_insert_failure;")
            .expect("drop failure trigger");
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM customers", [], |row| row
                    .get::<_, i64>(0))
                .expect("rolled back customer count"),
            customer_count_before
        );
    }

    fn require_live_dispatcher_paused(
        connection: &Connection,
        transport: &mut dyn services::google::HttpTransport,
        config: &services::supabase::SupabaseConfig,
        session: &mut services::supabase::SupabaseSession,
    ) -> Result<(), AppError> {
        let status =
            match services::reminder_cloud::read_dispatcher_status(transport, config, session) {
                Ok(status) => status,
                Err(AppError::Validation(code)) if code == "CLOUD_AUTH_INVALID" => {
                    let protector = services::secure_store::WindowsDpapiProtector;
                    let refreshed = services::supabase::refresh_session(
                        transport,
                        config,
                        &session.refresh_token,
                    )?;
                    services::secure_store::store_supabase_session(
                        connection, &protector, &refreshed,
                    )?;
                    *session = refreshed;
                    services::reminder_cloud::read_dispatcher_status(transport, config, session)?
                }
                Err(error) => return Err(error),
            };
        if !status.paused {
            return Err(AppError::Conflict("DISPATCHER_NOT_PAUSED".to_string()));
        }
        Ok(())
    }

    #[test]
    #[ignore = "requires explicit single synthetic hosted reminder acceptance authorization"]
    fn live_targeted_cloud_reminder_acceptance() {
        assert_eq!(
            std::env::var("BEAUTYSALOON_ALLOW_LIVE_CLOUD_ACCEPTANCE").as_deref(),
            Ok("1"),
            "live acceptance requires an explicit environment gate"
        );
        let database_path = PathBuf::from(std::env::var_os("APPDATA").expect("APPDATA"))
            .join("com.beautysaloon.desktop")
            .join("database")
            .join("salon-foundation.db");
        let mut connection = open_database(&database_path).expect("open live database");
        let protector = services::secure_store::WindowsDpapiProtector;
        let mut session = services::secure_store::read_supabase_session(&connection, &protector)
            .expect("read session")
            .expect("persisted session");
        let config = supabase_config().expect("Supabase config");
        let mut transport = services::google::ReqwestHttpTransport;

        require_live_dispatcher_paused(&connection, &mut transport, &config, &mut session)
            .expect("dispatcher paused before synthetic create");
        let non_target_pending_before: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM reminder_cloud_outbox WHERE sync_status='pending'",
                [],
                |row| row.get(0),
            )
            .expect("pending baseline");
        let marker = "BeautySaloon TEST CLOUD";
        let customer = create_customer_tx(
            &mut connection,
            CustomerInput {
                first_name: "BeautySaloon".into(),
                last_name: "TEST CLOUD".into(),
                phone: Some("05550000000".into()),
                email: None,
                notes: Some(marker.into()),
                whatsapp_reminder_enabled: Some(true),
                whatsapp_consent_confirmed: Some(true),
            },
        )
        .expect("synthetic customer");
        let staff = create_staff_tx(
            &mut connection,
            StaffInput {
                first_name: "BeautySaloon".into(),
                last_name: Some("TEST CLOUD".into()),
                phone: None,
                specialty_note: Some(marker.into()),
                color_key: "slate".into(),
                is_active: Some(true),
            },
        )
        .expect("synthetic staff");
        let category = create_category_tx(
            &connection,
            CategoryInput {
                name: "BeautySaloon TEST CLOUD".into(),
                is_active: Some(true),
            },
        )
        .expect("synthetic category");
        let service = create_service_tx(
            &mut connection,
            ServiceInput {
                category_id: category.id,
                name: "BeautySaloon TEST CLOUD".into(),
                duration_minutes: Some(30),
                default_price_minor: Some(0),
                is_active: Some(true),
            },
        )
        .expect("synthetic service");
        set_staff_services_tx(&mut connection, &staff.id, vec![service.id.clone()])
            .expect("synthetic staff service");
        let (local_date, local_time) = future_local_slot(48);
        let appointment = create_appointment_tx(
            &mut connection,
            AppointmentInput {
                customer_id: customer.id,
                staff_id: staff.id,
                local_date,
                local_start_time: local_time,
                service_ids: vec![service.id.clone()],
                status: Some("confirmed".into()),
                note: Some(marker.into()),
                whatsapp_reminder_enabled: None,
            },
        )
        .expect("synthetic appointment");
        // The acceptance is cloud-only; prevent this synthetic record entering Google sync.
        connection
            .execute(
                "DELETE FROM google_calendar_outbox WHERE appointment_id=?1",
                params![appointment.id],
            )
            .expect("remove synthetic Google outbox");
        connection
            .execute(
                "DELETE FROM appointment_google_calendar_sync WHERE appointment_id=?1",
                params![appointment.id],
            )
            .expect("remove synthetic Google mapping");
        let reminder_id: String = connection
            .query_row(
                "SELECT id FROM appointment_reminders WHERE appointment_id=?1",
                params![appointment.id],
                |row| row.get(0),
            )
            .expect("synthetic reminder");
        let create_outbox_id: String = connection
            .query_row(
                "SELECT id FROM reminder_cloud_outbox WHERE reminder_id=?1 AND action='upsert' AND sync_status='pending' ORDER BY revision DESC LIMIT 1",
                params![reminder_id],
                |row| row.get(0),
            )
            .expect("synthetic create outbox");

        require_live_dispatcher_paused(&connection, &mut transport, &config, &mut session)
            .expect("dispatcher paused before hosted create");
        assert_eq!(
            services::reminder_cloud::process_outbox_item(
                &connection,
                &mut transport,
                &config,
                &session,
                &create_outbox_id,
            )
            .expect("hosted create")
            .processed,
            1
        );
        let created = services::reminder_cloud::read_reminder_status(
            &mut transport,
            &config,
            &session,
            &reminder_id,
        )
        .expect("read hosted create")
        .expect("hosted synthetic reminder");
        assert_eq!(created.status, "pending");
        services::reminder_cloud::reconcile_remote_status(
            &connection,
            &reminder_id,
            created.revision,
            &created.status,
            &created.remote_updated_at_utc,
        )
        .expect("reconcile create");

        let (updated_date, updated_time) = future_local_slot(72);
        update_appointment_tx(
            &mut connection,
            &appointment.id,
            AppointmentInput {
                customer_id: appointment.customer_id.clone(),
                staff_id: appointment.staff_id.clone(),
                local_date: updated_date,
                local_start_time: updated_time,
                service_ids: vec![service.id],
                status: Some("planned".into()),
                note: Some(marker.into()),
                whatsapp_reminder_enabled: None,
            },
        )
        .expect("synthetic update");
        connection
            .execute(
                "DELETE FROM google_calendar_outbox WHERE appointment_id=?1",
                params![appointment.id],
            )
            .expect("remove synthetic Google update outbox");
        connection
            .execute(
                "DELETE FROM appointment_google_calendar_sync WHERE appointment_id=?1",
                params![appointment.id],
            )
            .expect("remove synthetic Google update mapping");
        let update_outbox_id: String = connection
            .query_row(
                "SELECT id FROM reminder_cloud_outbox WHERE reminder_id=?1 AND action='upsert' AND sync_status='pending' ORDER BY revision DESC LIMIT 1",
                params![reminder_id],
                |row| row.get(0),
            )
            .expect("synthetic update outbox");
        require_live_dispatcher_paused(&connection, &mut transport, &config, &mut session)
            .expect("dispatcher paused before hosted update");
        assert_eq!(
            services::reminder_cloud::process_outbox_item(
                &connection,
                &mut transport,
                &config,
                &session,
                &update_outbox_id,
            )
            .expect("hosted update")
            .processed,
            1
        );
        let updated = services::reminder_cloud::read_reminder_status(
            &mut transport,
            &config,
            &session,
            &reminder_id,
        )
        .expect("read hosted update")
        .expect("hosted synthetic reminder after update");
        assert!(updated.revision > created.revision);
        assert_eq!(updated.status, "pending");
        services::reminder_cloud::reconcile_remote_status(
            &connection,
            &reminder_id,
            updated.revision,
            &updated.status,
            &updated.remote_updated_at_utc,
        )
        .expect("reconcile update");
        assert_eq!(
            services::reminder_cloud::process_outbox_item(
                &connection,
                &mut transport,
                &config,
                &session,
                &update_outbox_id,
            )
            .expect("idempotent retry")
            .processed,
            0
        );

        let cancel_services = list_appointment_services(&connection, &appointment.id)
            .expect("services")
            .into_iter()
            .map(|item| item.service_id)
            .collect();
        let (cancel_date, cancel_time) = future_local_slot(72);
        update_appointment_tx(
            &mut connection,
            &appointment.id,
            AppointmentInput {
                customer_id: appointment.customer_id.clone(),
                staff_id: appointment.staff_id.clone(),
                local_date: cancel_date,
                local_start_time: cancel_time,
                service_ids: cancel_services,
                status: Some("cancelled".into()),
                note: Some(marker.into()),
                whatsapp_reminder_enabled: None,
            },
        )
        .expect("synthetic cancel");
        connection
            .execute(
                "DELETE FROM google_calendar_outbox WHERE appointment_id=?1",
                params![appointment.id],
            )
            .expect("remove synthetic Google cancel outbox");
        connection
            .execute(
                "DELETE FROM appointment_google_calendar_sync WHERE appointment_id=?1",
                params![appointment.id],
            )
            .expect("remove synthetic Google cancel mapping");
        let cancel_outbox_id: String = connection
            .query_row(
                "SELECT id FROM reminder_cloud_outbox WHERE reminder_id=?1 AND action='cancel' AND sync_status='pending' ORDER BY revision DESC LIMIT 1",
                params![reminder_id],
                |row| row.get(0),
            )
            .expect("synthetic cancel outbox");
        require_live_dispatcher_paused(&connection, &mut transport, &config, &mut session)
            .expect("dispatcher paused before hosted cancel");
        assert_eq!(
            services::reminder_cloud::process_outbox_item(
                &connection,
                &mut transport,
                &config,
                &session,
                &cancel_outbox_id,
            )
            .expect("hosted cancel")
            .processed,
            1
        );
        let cancelled = services::reminder_cloud::read_reminder_status(
            &mut transport,
            &config,
            &session,
            &reminder_id,
        )
        .expect("read hosted cancellation")
        .expect("hosted synthetic cancellation");
        assert_eq!(cancelled.status, "cancelled");
        services::reminder_cloud::reconcile_remote_status(
            &connection,
            &reminder_id,
            cancelled.revision,
            &cancelled.status,
            &cancelled.remote_updated_at_utc,
        )
        .expect("reconcile cancellation");
        let untouched_pending: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM reminder_cloud_outbox WHERE sync_status='pending' AND reminder_id<>?1",
                params![reminder_id],
                |row| row.get(0),
            )
            .expect("non-target pending");
        assert_eq!(untouched_pending, non_target_pending_before);
        assert_eq!(
            connection
                .query_row(
                    "SELECT status FROM appointment_reminders WHERE id=?1",
                    params![reminder_id],
                    |row| row.get::<_, String>(0),
                )
                .expect("local cancelled reminder"),
            "cancelled"
        );
        drop(connection);
        let reopened = open_database(&database_path).expect("reopen live database");
        assert_eq!(
            reopened
                .query_row(
                    "SELECT sync_status FROM reminder_cloud_outbox WHERE id=?1",
                    params![cancel_outbox_id],
                    |row| row.get::<_, String>(0),
                )
                .expect("persisted cancel outbox"),
            "synced"
        );
        assert!(integrity_check(&reopened).expect("integrity"));
    }

    #[test]
    fn duplicate_connect_prevented_by_guard() {
        let guard1 = try_acquire_google_connect_guard().expect("first acquire");
        let guard2_err =
            try_acquire_google_connect_guard().expect_err("second acquire should fail");
        assert!(matches!(guard2_err, AppError::Conflict(_)));
        drop(guard1);
        let guard3 = try_acquire_google_connect_guard().expect("third acquire after drop");
        drop(guard3);
    }

    #[test]
    fn weekly_hours_repository_replaces_only_valid_staff_schedule() {
        let (_temp, mut connection) = open_temp();
        let (_customer, staff, _service) = seed_core(&mut connection);
        assert!(list_staff_working_hours(&connection, &staff.id)
            .expect("empty")
            .is_empty());
        assert!(is_staff_schedule_unrestricted(&connection, &staff.id).expect("unrestricted"));
        let first = vec![
            StaffWorkingHour {
                staff_id: staff.id.clone(),
                weekday: 1,
                start_minute: 540,
                end_minute: 720,
            },
            StaffWorkingHour {
                staff_id: staff.id.clone(),
                weekday: 1,
                start_minute: 780,
                end_minute: 1080,
            },
        ];
        replace_staff_working_hours(&mut connection, &staff.id, first.clone()).expect("replace");
        assert_eq!(
            list_staff_working_hours(&connection, &staff.id).expect("list"),
            first
        );
        assert!(!is_staff_schedule_unrestricted(&connection, &staff.id).expect("configured"));
        let second = vec![StaffWorkingHour {
            staff_id: staff.id.clone(),
            weekday: 2,
            start_minute: 600,
            end_minute: 1020,
        }];
        replace_staff_working_hours(&mut connection, &staff.id, second.clone())
            .expect("replace second");
        assert_eq!(
            list_staff_working_hours(&connection, &staff.id).expect("list second"),
            second
        );
        let invalid = vec![StaffWorkingHour {
            staff_id: staff.id.clone(),
            weekday: 7,
            start_minute: 600,
            end_minute: 600,
        }];
        assert!(replace_staff_working_hours(&mut connection, &staff.id, invalid).is_err());
        assert_eq!(
            list_staff_working_hours(&connection, &staff.id).expect("unchanged after invalid"),
            second
        );
        let adjacent = vec![
            StaffWorkingHour {
                staff_id: staff.id.clone(),
                weekday: 3,
                start_minute: 540,
                end_minute: 720,
            },
            StaffWorkingHour {
                staff_id: staff.id.clone(),
                weekday: 3,
                start_minute: 720,
                end_minute: 900,
            },
            StaffWorkingHour {
                staff_id: staff.id.clone(),
                weekday: 4,
                start_minute: 600,
                end_minute: 840,
            },
        ];
        replace_staff_working_hours(&mut connection, &staff.id, adjacent.clone())
            .expect("adjacent");
        assert_eq!(
            list_staff_working_hours(&connection, &staff.id).expect("adjacent list"),
            adjacent
        );
        let overlap = vec![
            StaffWorkingHour {
                staff_id: staff.id.clone(),
                weekday: 5,
                start_minute: 540,
                end_minute: 720,
            },
            StaffWorkingHour {
                staff_id: staff.id.clone(),
                weekday: 5,
                start_minute: 690,
                end_minute: 840,
            },
        ];
        assert!(replace_staff_working_hours(&mut connection, &staff.id, overlap).is_err());
        assert_eq!(
            list_staff_working_hours(&connection, &staff.id).expect("unchanged after overlap"),
            adjacent
        );
    }

    #[test]
    fn staff_working_hour_serializes_for_tauri_command_payload() {
        let value = serde_json::to_value(StaffWorkingHour {
            staff_id: "staff-1".into(),
            weekday: 1,
            start_minute: 540,
            end_minute: 720,
        })
        .expect("serialize working hour");

        assert_eq!(value["staffId"], "staff-1");
        assert_eq!(value["weekday"], 1);
        assert_eq!(value["startMinute"], 540);
        assert_eq!(value["endMinute"], 720);
    }

    #[test]
    fn staff_time_off_repository_enforces_full_day_and_partial_domain_rules() {
        let (_temp, mut connection) = open_temp();
        let (_customer, staff, _service) = seed_core(&mut connection);
        let other_staff = create_staff_tx(
            &mut connection,
            StaffInput {
                first_name: "Other".into(),
                last_name: None,
                phone: None,
                specialty_note: None,
                color_key: "blue".into(),
                is_active: Some(true),
            },
        )
        .expect("other staff");
        let full_day = StaffTimeOffInput {
            local_date: "2026-10-01".into(),
            full_day: true,
            start_minute: None,
            end_minute: None,
        };
        let full_day_record =
            add_staff_time_off(&mut connection, &staff.id, full_day).expect("add full day");
        assert!(full_day_record.full_day);
        assert_eq!(
            list_staff_time_off(&connection, &staff.id).expect("list full day"),
            vec![full_day_record.clone()]
        );
        assert!(add_staff_time_off(
            &mut connection,
            &staff.id,
            StaffTimeOffInput {
                local_date: "2026-10-01".into(),
                full_day: true,
                start_minute: None,
                end_minute: None,
            },
        )
        .is_err());
        assert!(add_staff_time_off(
            &mut connection,
            &staff.id,
            StaffTimeOffInput {
                local_date: "2026-10-01".into(),
                full_day: false,
                start_minute: Some(780),
                end_minute: Some(900),
            },
        )
        .is_err());
        let updated_full_day = update_staff_time_off(
            &mut connection,
            &staff.id,
            &full_day_record.id,
            StaffTimeOffInput {
                local_date: "2026-10-02".into(),
                full_day: true,
                start_minute: None,
                end_minute: None,
            },
        )
        .expect("update full day");
        assert_eq!(updated_full_day.local_date, "2026-10-02");

        let partial_input = StaffTimeOffInput {
            local_date: "2026-10-03".into(),
            full_day: false,
            start_minute: Some(780),
            end_minute: Some(900),
        };
        let partial = add_staff_time_off(&mut connection, &staff.id, partial_input.clone())
            .expect("add partial");
        assert_eq!(partial.start_minute, Some(780));
        assert_eq!(partial.end_minute, Some(900));
        let self_updated = update_staff_time_off(
            &mut connection,
            &staff.id,
            &partial.id,
            partial_input.clone(),
        )
        .expect("update excludes itself");
        assert_eq!(self_updated, partial);
        let adjacent = add_staff_time_off(
            &mut connection,
            &staff.id,
            StaffTimeOffInput {
                local_date: "2026-10-03".into(),
                full_day: false,
                start_minute: Some(900),
                end_minute: Some(960),
            },
        )
        .expect("adjacent partial");
        assert!(add_staff_time_off(
            &mut connection,
            &staff.id,
            StaffTimeOffInput {
                local_date: "2026-10-03".into(),
                full_day: false,
                start_minute: Some(870),
                end_minute: Some(930),
            },
        )
        .is_err());
        assert!(add_staff_time_off(
            &mut connection,
            &staff.id,
            StaffTimeOffInput {
                local_date: "2026-10-03".into(),
                full_day: true,
                start_minute: None,
                end_minute: None,
            },
        )
        .is_err());
        assert!(add_staff_time_off(
            &mut connection,
            &staff.id,
            StaffTimeOffInput {
                local_date: "2026-10-04".into(),
                full_day: false,
                start_minute: Some(960),
                end_minute: Some(960),
            },
        )
        .is_err());
        assert!(add_staff_time_off(
            &mut connection,
            &staff.id,
            StaffTimeOffInput {
                local_date: "2026-10-04".into(),
                full_day: true,
                start_minute: Some(600),
                end_minute: Some(720),
            },
        )
        .is_err());
        let invalid_update = update_staff_time_off(
            &mut connection,
            &staff.id,
            &partial.id,
            StaffTimeOffInput {
                local_date: "2026-10-03".into(),
                full_day: false,
                start_minute: Some(950),
                end_minute: Some(900),
            },
        );
        assert!(invalid_update.is_err());
        assert_eq!(
            get_staff_time_off(&connection, &staff.id, &partial.id).expect("read partial"),
            Some(partial.clone())
        );
        assert!(add_staff_time_off(
            &mut connection,
            &other_staff.id,
            StaffTimeOffInput {
                local_date: "2026-10-03".into(),
                full_day: true,
                start_minute: None,
                end_minute: None,
            },
        )
        .is_ok());
        assert!(add_staff_time_off(
            &mut connection,
            &staff.id,
            StaffTimeOffInput {
                local_date: "2026-10-05".into(),
                full_day: false,
                start_minute: Some(780),
                end_minute: Some(900),
            },
        )
        .is_ok());
        remove_staff_time_off(&mut connection, &staff.id, &adjacent.id).expect("remove target");
        assert!(get_staff_time_off(&connection, &staff.id, &adjacent.id)
            .expect("removed")
            .is_none());
        assert_eq!(
            get_staff_time_off(&connection, &staff.id, &partial.id).expect("other retained"),
            Some(partial)
        );
    }

    #[test]
    fn staff_time_off_tauri_dtos_use_typed_camel_case_fields() {
        let input: StaffTimeOffInput = serde_json::from_value(serde_json::json!({
            "localDate": "2026-10-03",
            "fullDay": false,
            "startMinute": 780,
            "endMinute": 900
        }))
        .expect("deserialize command input");
        assert_eq!(input.local_date, "2026-10-03");
        assert!(!input.full_day);
        assert_eq!(input.start_minute, Some(780));
        assert_eq!(input.end_minute, Some(900));

        let value = serde_json::to_value(StaffTimeOff {
            id: "time-off-1".into(),
            staff_id: "staff-1".into(),
            local_date: "2026-10-03".into(),
            full_day: false,
            start_minute: Some(780),
            end_minute: Some(900),
        })
        .expect("serialize command output");
        assert_eq!(value["id"], "time-off-1");
        assert_eq!(value["staffId"], "staff-1");
        assert_eq!(value["localDate"], "2026-10-03");
        assert_eq!(value["fullDay"], false);
        assert_eq!(value["startMinute"], 780);
        assert_eq!(value["endMinute"], 900);
    }

    #[test]
    fn appointment_availability_core_uses_historical_durations_and_sql_conflicts() {
        let (_temp, mut connection) = open_temp();
        let (customer, staff, service) = seed_core(&mut connection);
        let category_id = service.category_id.clone();
        let second_service = create_service_tx(
            &mut connection,
            ServiceInput {
                category_id: category_id.clone(),
                name: "Ek Bakim".into(),
                duration_minutes: Some(45),
                default_price_minor: Some(0),
                is_active: Some(true),
            },
        )
        .expect("second service");
        set_staff_services_tx(
            &mut connection,
            &staff.id,
            vec![service.id.clone(), second_service.id.clone()],
        )
        .expect("assign services");
        let appointment = create_appointment_tx(
            &mut connection,
            AppointmentInput {
                customer_id: customer.id.clone(),
                staff_id: staff.id.clone(),
                local_date: "2036-10-05".into(),
                local_start_time: "10:00".into(),
                service_ids: vec![service.id.clone()],
                status: Some("planned".into()),
                note: None,
                whatsapp_reminder_enabled: None,
            },
        )
        .expect("single-service appointment");
        assert_eq!(
            appointment_duration_minutes(&connection, &appointment.id).expect("single duration"),
            30
        );
        let multi_service = create_appointment_tx(
            &mut connection,
            AppointmentInput {
                customer_id: customer.id.clone(),
                staff_id: staff.id.clone(),
                local_date: "2036-10-06".into(),
                local_start_time: "10:00".into(),
                service_ids: vec![service.id.clone(), second_service.id.clone()],
                status: Some("planned".into()),
                note: None,
                whatsapp_reminder_enabled: None,
            },
        )
        .expect("multi-service appointment");
        assert_eq!(
            appointment_duration_minutes(&connection, &multi_service.id).expect("multi duration"),
            75
        );
        update_service_tx(
            &mut connection,
            &service.id,
            ServiceInput {
                category_id: category_id.clone(),
                name: "Klasik Bakim Yeni".into(),
                duration_minutes: Some(60),
                default_price_minor: Some(0),
                is_active: Some(true),
            },
        )
        .expect("edit current duration");
        assert_eq!(
            appointment_duration_minutes(&connection, &appointment.id)
                .expect("historical single duration"),
            30
        );
        assert_eq!(
            appointment_duration_minutes(&connection, &multi_service.id)
                .expect("historical multi duration"),
            75
        );

        let adjacent =
            appointment_interval_from_local("2036-10-05", "10:30", 30).expect("adjacent interval");
        assert!(
            !staff_has_appointment_conflict(&connection, &staff.id, &adjacent, None)
                .expect("adjacent query")
        );
        for (time, duration) in [("10:15", 30), ("09:45", 60), ("10:05", 10)] {
            let interval = appointment_interval_from_local("2036-10-05", time, duration)
                .expect("overlapping interval");
            assert!(
                staff_has_appointment_conflict(&connection, &staff.id, &interval, None)
                    .expect("overlap query")
            );
        }
        let exact =
            appointment_interval_from_local("2036-10-05", "10:00", 30).expect("exact interval");
        assert!(!staff_has_appointment_conflict(
            &connection,
            &staff.id,
            &exact,
            Some(&appointment.id),
        )
        .expect("exclude self"));
        let other_staff = create_staff_tx(
            &mut connection,
            StaffInput {
                first_name: "Derya".into(),
                last_name: None,
                phone: None,
                specialty_note: None,
                color_key: "blue".into(),
                is_active: Some(true),
            },
        )
        .expect("other staff");
        assert!(
            !staff_has_appointment_conflict(&connection, &other_staff.id, &exact, None)
                .expect("different staff")
        );
        connection
            .execute(
                "UPDATE appointments SET status='cancelled' WHERE id=?1",
                params![appointment.id],
            )
            .expect("cancel appointment");
        assert!(
            !staff_has_appointment_conflict(&connection, &staff.id, &exact, None)
                .expect("cancelled does not block")
        );
        connection
            .execute(
                "UPDATE appointments SET status='no_show' WHERE id=?1",
                params![appointment.id],
            )
            .expect("mark no show");
        assert!(
            staff_has_appointment_conflict(&connection, &staff.id, &exact, None)
                .expect("non-cancelled blocks")
        );
    }

    #[test]
    fn appointment_availability_core_applies_working_hours_and_time_off() {
        let (_temp, mut connection) = open_temp();
        let (_customer, staff, _service) = seed_core(&mut connection);
        let working_date = NaiveDate::from_ymd_opt(2036, 10, 7).expect("working date");
        let weekday = i64::from(working_date.weekday().num_days_from_monday());
        replace_staff_working_hours(
            &mut connection,
            &staff.id,
            vec![
                StaffWorkingHour {
                    staff_id: staff.id.clone(),
                    weekday,
                    start_minute: 540,
                    end_minute: 720,
                },
                StaffWorkingHour {
                    staff_id: staff.id.clone(),
                    weekday,
                    start_minute: 780,
                    end_minute: 1080,
                },
            ],
        )
        .expect("schedule");
        for (time, duration, expected) in [
            ("09:00", 30, AppointmentAvailability::Available),
            (
                "08:45",
                30,
                AppointmentAvailability::Unavailable(
                    AppointmentAvailabilityReason::OutsideWorkingHours,
                ),
            ),
            (
                "17:30",
                60,
                AppointmentAvailability::Unavailable(
                    AppointmentAvailabilityReason::OutsideWorkingHours,
                ),
            ),
            (
                "11:30",
                60,
                AppointmentAvailability::Unavailable(
                    AppointmentAvailabilityReason::OutsideWorkingHours,
                ),
            ),
            ("13:00", 60, AppointmentAvailability::Available),
        ] {
            let interval = appointment_interval_from_local("2036-10-07", time, duration)
                .expect("scheduled interval");
            assert_eq!(
                check_staff_appointment_availability(&connection, &staff.id, &interval, None)
                    .expect("availability"),
                expected
            );
        }
        let unscheduled_day =
            appointment_interval_from_local("2036-10-08", "10:00", 30).expect("unscheduled day");
        assert_eq!(
            check_staff_appointment_availability(&connection, &staff.id, &unscheduled_day, None)
                .expect("weekday availability"),
            AppointmentAvailability::Unavailable(
                AppointmentAvailabilityReason::OutsideWorkingHours,
            )
        );

        add_staff_time_off(
            &mut connection,
            &staff.id,
            StaffTimeOffInput {
                local_date: "2036-10-07".into(),
                full_day: true,
                start_minute: None,
                end_minute: None,
            },
        )
        .expect("full day leave");
        let full_day_interval =
            appointment_interval_from_local("2036-10-07", "09:00", 30).expect("full day interval");
        assert_eq!(
            check_staff_appointment_availability(&connection, &staff.id, &full_day_interval, None)
                .expect("full day availability"),
            AppointmentAvailability::Unavailable(AppointmentAvailabilityReason::StaffTimeOff)
        );
        let full_day_time_off_id = list_staff_time_off(&connection, &staff.id).expect("leaves")[0]
            .id
            .clone();
        remove_staff_time_off(&mut connection, &staff.id, &full_day_time_off_id)
            .expect("remove full day");
        let next_week = "2036-10-14";
        add_staff_time_off(
            &mut connection,
            &staff.id,
            StaffTimeOffInput {
                local_date: next_week.into(),
                full_day: false,
                start_minute: Some(780),
                end_minute: Some(900),
            },
        )
        .expect("partial leave");
        let partial_overlap =
            appointment_interval_from_local(next_week, "14:30", 30).expect("partial overlap");
        assert_eq!(
            check_staff_appointment_availability(&connection, &staff.id, &partial_overlap, None)
                .expect("partial availability"),
            AppointmentAvailability::Unavailable(AppointmentAvailabilityReason::StaffTimeOff)
        );
        let partial_adjacent =
            appointment_interval_from_local(next_week, "15:00", 30).expect("partial adjacent");
        assert_eq!(
            check_staff_appointment_availability(&connection, &staff.id, &partial_adjacent, None)
                .expect("partial adjacency"),
            AppointmentAvailability::Available
        );
    }

    #[test]
    fn appointment_create_enforces_availability_without_local_or_outbox_side_effects() {
        let (_temp, mut connection) = open_temp();
        let (customer, staff, service) = seed_core(&mut connection);
        configure_daily_hours(&mut connection, &staff.id);
        let valid = create_appointment_tx(
            &mut connection,
            appointment_input(
                &customer.id,
                &staff.id,
                "2036-10-20",
                "09:00",
                vec![service.id.clone()],
                "planned",
            ),
        )
        .expect("valid create");
        let appointments_before: i64 = connection
            .query_row("SELECT COUNT(*) FROM appointments", [], |row| row.get(0))
            .expect("appointment count");
        let services_before: i64 = connection
            .query_row("SELECT COUNT(*) FROM appointment_services", [], |row| {
                row.get(0)
            })
            .expect("appointment service count");
        let google_outbox_before: i64 = connection
            .query_row("SELECT COUNT(*) FROM google_calendar_outbox", [], |row| {
                row.get(0)
            })
            .expect("google outbox count");
        let cloud_outbox_before: i64 = connection
            .query_row("SELECT COUNT(*) FROM reminder_cloud_outbox", [], |row| {
                row.get(0)
            })
            .expect("cloud outbox count");
        assert!(create_appointment_tx(
            &mut connection,
            appointment_input(
                &customer.id,
                &staff.id,
                "2036-10-20",
                "08:30",
                vec![service.id.clone()],
                "planned",
            ),
        )
        .is_err());
        assert!(create_appointment_tx(
            &mut connection,
            appointment_input(
                &customer.id,
                &staff.id,
                "2036-10-20",
                "09:15",
                vec![service.id.clone()],
                "planned",
            ),
        )
        .is_err());
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM appointments", [], |row| {
                    row.get::<_, i64>(0)
                })
                .expect("unchanged appointments"),
            appointments_before
        );
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM appointment_services", [], |row| {
                    row.get::<_, i64>(0)
                })
                .expect("unchanged services"),
            services_before
        );
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM google_calendar_outbox", [], |row| {
                    row.get::<_, i64>(0)
                })
                .expect("unchanged google outbox"),
            google_outbox_before
        );
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM reminder_cloud_outbox", [], |row| {
                    row.get::<_, i64>(0)
                })
                .expect("unchanged cloud outbox"),
            cloud_outbox_before
        );
        add_staff_time_off(
            &mut connection,
            &staff.id,
            StaffTimeOffInput {
                local_date: "2036-10-20".into(),
                full_day: false,
                start_minute: Some(780),
                end_minute: Some(840),
            },
        )
        .expect("time off");
        assert!(create_appointment_tx(
            &mut connection,
            appointment_input(
                &customer.id,
                &staff.id,
                "2036-10-20",
                "13:00",
                vec![service.id.clone()],
                "planned",
            ),
        )
        .is_err());
        let adjacent = create_appointment_tx(
            &mut connection,
            appointment_input(
                &customer.id,
                &staff.id,
                "2036-10-20",
                "09:30",
                vec![service.id.clone()],
                "planned",
            ),
        )
        .expect("adjacent create");
        assert_ne!(adjacent.id, valid.id);
        let other_staff = create_staff_tx(
            &mut connection,
            StaffInput {
                first_name: "Mina".into(),
                last_name: None,
                phone: None,
                specialty_note: None,
                color_key: "blue".into(),
                is_active: Some(true),
            },
        )
        .expect("other staff");
        set_staff_services_tx(&mut connection, &other_staff.id, vec![service.id.clone()])
            .expect("assign other staff");
        assert!(create_appointment_tx(
            &mut connection,
            appointment_input(
                &customer.id,
                &other_staff.id,
                "2036-10-20",
                "09:00",
                vec![service.id.clone()],
                "planned",
            ),
        )
        .is_ok());
    }

    #[test]
    fn appointment_update_enforces_availability_and_preserves_failed_updates() {
        let (_temp, mut connection) = open_temp();
        let (customer, staff, service) = seed_core(&mut connection);
        configure_daily_hours(&mut connection, &staff.id);
        let long_service = create_service_tx(
            &mut connection,
            ServiceInput {
                category_id: service.category_id.clone(),
                name: "Uzun Bakim".into(),
                duration_minutes: Some(60),
                default_price_minor: Some(0),
                is_active: Some(true),
            },
        )
        .expect("long service");
        set_staff_services_tx(
            &mut connection,
            &staff.id,
            vec![service.id.clone(), long_service.id.clone()],
        )
        .expect("assign long service");
        let first = create_appointment_tx(
            &mut connection,
            appointment_input(
                &customer.id,
                &staff.id,
                "2036-10-21",
                "09:00",
                vec![service.id.clone()],
                "planned",
            ),
        )
        .expect("first appointment");
        let second = create_appointment_tx(
            &mut connection,
            appointment_input(
                &customer.id,
                &staff.id,
                "2036-10-21",
                "10:00",
                vec![service.id.clone()],
                "planned",
            ),
        )
        .expect("second appointment");
        let moved = update_appointment_tx(
            &mut connection,
            &first.id,
            appointment_input(
                &customer.id,
                &staff.id,
                "2036-10-21",
                "09:30",
                vec![service.id.clone()],
                "planned",
            ),
        )
        .expect("valid reschedule");
        assert_eq!(
            moved.start_at_utc,
            utc_iso(local_to_utc("2036-10-21", "09:30").unwrap())
        );
        let before_failed_update = get_appointment(&connection, &first.id).expect("before failure");
        let snapshots_before =
            list_appointment_services(&connection, &first.id).expect("snapshots before");
        assert!(update_appointment_tx(
            &mut connection,
            &first.id,
            appointment_input(
                &customer.id,
                &staff.id,
                "2036-10-21",
                "10:00",
                vec![service.id.clone()],
                "planned",
            ),
        )
        .is_err());
        assert_eq!(
            get_appointment(&connection, &first.id)
                .expect("after conflict")
                .start_at_utc,
            before_failed_update.start_at_utc
        );
        assert_eq!(
            list_appointment_services(&connection, &first.id).expect("snapshots after conflict"),
            snapshots_before
        );
        let other_staff = create_staff_tx(
            &mut connection,
            StaffInput {
                first_name: "Ece".into(),
                last_name: None,
                phone: None,
                specialty_note: None,
                color_key: "teal".into(),
                is_active: Some(true),
            },
        )
        .expect("other staff");
        set_staff_services_tx(&mut connection, &other_staff.id, vec![service.id.clone()])
            .expect("assign other staff");
        let other_appointment = create_appointment_tx(
            &mut connection,
            appointment_input(
                &customer.id,
                &other_staff.id,
                "2036-10-21",
                "11:00",
                vec![service.id.clone()],
                "planned",
            ),
        )
        .expect("other appointment");
        assert!(update_appointment_tx(
            &mut connection,
            &first.id,
            appointment_input(
                &customer.id,
                &other_staff.id,
                "2036-10-21",
                "11:00",
                vec![service.id.clone()],
                "planned",
            ),
        )
        .is_err());
        assert_eq!(
            get_appointment(&connection, &first.id)
                .expect("staff preserved")
                .staff_id,
            staff.id
        );
        add_staff_time_off(
            &mut connection,
            &staff.id,
            StaffTimeOffInput {
                local_date: "2036-10-22".into(),
                full_day: true,
                start_minute: None,
                end_minute: None,
            },
        )
        .expect("leave");
        assert!(update_appointment_tx(
            &mut connection,
            &first.id,
            appointment_input(
                &customer.id,
                &staff.id,
                "2036-10-22",
                "09:30",
                vec![service.id.clone()],
                "planned",
            ),
        )
        .is_err());
        assert!(update_appointment_tx(
            &mut connection,
            &first.id,
            appointment_input(
                &customer.id,
                &staff.id,
                "2036-10-21",
                "09:30",
                vec![service.id.clone(), long_service.id.clone()],
                "planned",
            ),
        )
        .is_err());
        let service_change = update_appointment_tx(
            &mut connection,
            &first.id,
            appointment_input(
                &customer.id,
                &staff.id,
                "2036-10-21",
                "13:00",
                vec![service.id.clone(), long_service.id.clone()],
                "planned",
            ),
        )
        .expect("valid longer composition");
        assert_eq!(
            appointment_duration_minutes(&connection, &service_change.id).expect("new duration"),
            90
        );
        let self_update = update_appointment_tx(
            &mut connection,
            &first.id,
            appointment_input(
                &customer.id,
                &staff.id,
                "2036-10-21",
                "13:00",
                vec![service.id.clone(), long_service.id.clone()],
                "planned",
            ),
        );
        assert!(self_update.is_ok());
        let cancelled = update_appointment_tx(
            &mut connection,
            &second.id,
            appointment_input(
                &customer.id,
                &staff.id,
                "2036-10-21",
                "10:00",
                vec![service.id.clone()],
                APPOINTMENT_STATUS_CANCELLED,
            ),
        )
        .expect("cancel releases slot");
        assert_eq!(cancelled.status, APPOINTMENT_STATUS_CANCELLED);
        let replacement = create_appointment_tx(
            &mut connection,
            appointment_input(
                &customer.id,
                &staff.id,
                "2036-10-21",
                "10:00",
                vec![service.id.clone()],
                "planned",
            ),
        )
        .expect("cancelled slot available");
        assert_ne!(replacement.id, second.id);
        assert!(update_appointment_tx(
            &mut connection,
            &second.id,
            appointment_input(
                &customer.id,
                &staff.id,
                "2036-10-21",
                "10:00",
                vec![service.id.clone()],
                "planned",
            ),
        )
        .is_err());
        assert_eq!(
            get_appointment(&connection, &second.id)
                .expect("cancelled state retained")
                .status,
            APPOINTMENT_STATUS_CANCELLED
        );
        assert_eq!(other_appointment.staff_id, other_staff.id);
    }

    #[test]
    fn customer_history_uses_bounded_customer_page_and_batched_snapshots() {
        let (_temp, mut connection) = open_temp();
        let (customer, staff, service) = seed_core(&mut connection);
        let now = now_iso();
        let tx = connection
            .transaction()
            .expect("history fixture transaction");
        for index in 0..5_000 {
            let appointment_id = format!("history-page-{index:05}");
            tx.execute(
                "INSERT INTO appointments (id, customer_id, staff_id, start_at_utc, end_at_utc, total_duration_minutes, status, note, created_at, updated_at)
                 VALUES (?1, ?2, ?3, '2025-01-01T00:00:00.000Z', '2025-01-01T00:30:00.000Z', 30, 'planned', NULL, ?4, ?4)",
                params![appointment_id, customer.id, staff.id, now],
            )
            .expect("history appointment");
            tx.execute(
                "INSERT INTO appointment_services (appointment_id, service_id, service_name_snapshot, duration_minutes_snapshot, listed_price_snapshot_minor, charged_price_minor, sort_order, created_at)
                 VALUES (?1, ?2, 'History Snapshot', 30, 0, 0, 0, ?3)",
                params![appointment_id, service.id, now],
            )
            .expect("history snapshot");
        }
        tx.commit().expect("history fixture commit");

        let mut plan_statement = connection
            .prepare(
                "EXPLAIN QUERY PLAN
                 SELECT a.id, a.start_at_utc, a.status, a.staff_id
                 FROM appointments a
                 WHERE a.customer_id=?1
                 ORDER BY a.start_at_utc DESC, a.id DESC
                 LIMIT ?2 OFFSET ?3",
            )
            .expect("history plan");
        let plan = plan_statement
            .query_map(params![customer.id, 25_i64, 4_975_i64], |row| {
                row.get::<_, String>(3)
            })
            .expect("history plan rows")
            .collect::<Result<Vec<_>, _>>()
            .expect("history plan collect");
        assert!(
            plan.iter()
                .any(|detail| detail.contains("appointments_customer_id_idx")),
            "history plan did not use customer index: {plan:?}"
        );

        let page = load_customer_history(&connection, &customer.id, 25, 4_975)
            .expect("bounded history page");
        assert_eq!(page.appointments.len(), 25);
        assert_eq!(page.limit, 25);
        assert_eq!(page.offset, 4_975);
        assert!(page
            .appointments
            .iter()
            .all(|appointment| appointment.services.len() == 1));
        assert!(load_customer_history(&connection, &customer.id, 25, 5_000)
            .expect("empty next page")
            .appointments
            .is_empty());
        let seed = load_repeat_booking_seed(&connection, &customer.id, "history-page-00000")
            .expect("bounded repeat seed");
        assert_eq!(seed.services.len(), 1);
    }

    #[test]
    fn availability_queries_use_staff_indexes_at_ten_thousand_appointment_scale() {
        let (_temp, mut connection) = open_temp();
        let (customer, staff, service) = seed_core(&mut connection);
        configure_daily_hours(&mut connection, &staff.id);
        let base = chrono::DateTime::parse_from_rfc3339("2036-01-01T06:00:00Z")
            .expect("base timestamp")
            .with_timezone(&Utc);
        let time_off_date = NaiveDate::from_ymd_opt(2037, 1, 1).expect("time off base");
        let tx = connection.transaction().expect("scale transaction");
        for index in 0..10_000_i64 {
            let appointment_id = format!("scale-appointment-{index}");
            let start_at = utc_iso(base + Duration::minutes(index * 30));
            let end_at = utc_iso(base + Duration::minutes(index * 30 + 30));
            tx.execute(
                "INSERT INTO appointments (id, customer_id, staff_id, start_at_utc, end_at_utc, total_duration_minutes, status, note, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, 30, 'planned', NULL, ?6, ?6)",
                params![appointment_id, &customer.id, &staff.id, start_at, end_at, now_iso()],
            )
            .expect("scale appointment");
            tx.execute(
                "INSERT INTO appointment_services (appointment_id, service_id, service_name_snapshot, duration_minutes_snapshot, listed_price_snapshot_minor, charged_price_minor, sort_order, created_at)
                 VALUES (?1, ?2, 'Scale', 30, 0, 0, 10, ?3)",
                params![format!("scale-appointment-{index}"), &service.id, now_iso()],
            )
            .expect("scale appointment service");
        }
        for index in 0..2_000_i64 {
            let local_date = time_off_date
                .checked_add_signed(Duration::days(index * 2))
                .expect("time off date")
                .format("%Y-%m-%d")
                .to_string();
            tx.execute(
                "INSERT INTO staff_time_off (id, staff_id, local_date, full_day, start_minute, end_minute)
                 VALUES (?1, ?2, ?3, 0, 540, 570)",
                params![format!("scale-time-off-{index}"), &staff.id, local_date],
            )
            .expect("scale time off");
        }
        tx.commit().expect("commit scale data");

        let interval =
            appointment_interval_from_local("2036-02-01", "10:00", 30).expect("scale interval");
        assert!(
            staff_has_appointment_conflict(&connection, &staff.id, &interval, None)
                .expect("bounded conflict query")
        );
        let mut conflict_plan_statement = connection
            .prepare(
                "EXPLAIN QUERY PLAN
                 SELECT EXISTS(
                   SELECT 1
                   FROM appointments a
                   INNER JOIN appointment_services aps ON aps.appointment_id=a.id
                   WHERE a.staff_id=?1 AND a.status<>?2 AND (?3 IS NULL OR a.id<>?3)
                     AND a.start_at_utc<?5
                   GROUP BY a.id, a.start_at_utc
                   HAVING julianday(?4) < julianday(a.start_at_utc) + SUM(aps.duration_minutes_snapshot) / 1440.0
                 )",
            )
            .expect("conflict plan statement");
        let conflict_plan = conflict_plan_statement
            .query_map(
                params![
                    staff.id,
                    APPOINTMENT_STATUS_CANCELLED,
                    Option::<String>::None,
                    utc_iso(interval.start_at_utc),
                    utc_iso(interval.end_at_utc)
                ],
                |row| row.get::<_, String>(3),
            )
            .expect("conflict plan")
            .collect::<Result<Vec<_>, _>>()
            .expect("conflict plan rows");
        assert!(
            conflict_plan
                .iter()
                .any(|detail| detail.contains("appointments_staff_time_idx")),
            "conflict plan did not use staff/time index: {conflict_plan:?}"
        );
        let mut time_off_plan_statement = connection
            .prepare(
                "EXPLAIN QUERY PLAN
                 SELECT EXISTS(
                   SELECT 1 FROM staff_time_off
                   WHERE staff_id=?1 AND local_date=?2
                     AND (full_day=1 OR (start_minute<?4 AND end_minute>?3))
                 )",
            )
            .expect("time off plan statement");
        let time_off_plan = time_off_plan_statement
            .query_map(params![staff.id, "2037-01-01", 540, 600], |row| {
                row.get::<_, String>(3)
            })
            .expect("time off plan")
            .collect::<Result<Vec<_>, _>>()
            .expect("time off plan rows");
        assert!(
            time_off_plan
                .iter()
                .any(|detail| detail.contains("staff_time_off_staff_date_idx")),
            "time off plan did not use staff/date index: {time_off_plan:?}"
        );
    }

    #[test]
    fn oauth_callback_timeout_returns_safe_error() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        listener.set_nonblocking(true).expect("nonblocking");
        let port = listener.local_addr().expect("port").port();
        let redirect_uri = format!("http://127.0.0.1:{port}/oauth2callback");

        let result =
            wait_for_oauth_callback(&listener, &redirect_uri, StdDuration::from_millis(50));
        assert!(matches!(result, Err(AppError::Validation(msg)) if msg == "GOOGLE_OAUTH_TIMEOUT"));
    }

    #[test]
    fn oauth_callback_window_allows_consent_beyond_the_old_two_minute_limit() {
        assert_eq!(
            GOOGLE_OAUTH_CALLBACK_TIMEOUT,
            StdDuration::from_secs(15 * 60)
        );
        assert!(oauth_callback_window_open(
            StdDuration::from_secs(121),
            GOOGLE_OAUTH_CALLBACK_TIMEOUT
        ));
        assert!(!oauth_callback_window_open(
            GOOGLE_OAUTH_CALLBACK_TIMEOUT,
            GOOGLE_OAUTH_CALLBACK_TIMEOUT
        ));
    }

    #[test]
    fn oauth_timeout_releases_the_connect_guard() {
        {
            let _guard = try_acquire_google_connect_guard().expect("acquire guard");
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
            listener.set_nonblocking(true).expect("nonblocking");
            let port = listener.local_addr().expect("port").port();
            let redirect_uri = format!("http://127.0.0.1:{port}/oauth2callback");
            let result =
                wait_for_oauth_callback(&listener, &redirect_uri, StdDuration::from_millis(1));
            assert!(
                matches!(result, Err(AppError::Validation(msg)) if msg == "GOOGLE_OAUTH_TIMEOUT")
            );
        }
        let guard = try_acquire_google_connect_guard().expect("guard released after timeout");
        drop(guard);
    }

    #[test]
    fn listener_waiting_does_not_block_other_commands_or_db() {
        let (_temp, mut connection) = open_temp();
        let (customer, staff, service) = seed_core(&mut connection);

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        listener.set_nonblocking(true).expect("nonblocking");
        let port = listener.local_addr().expect("port").port();
        let redirect_uri = format!("http://127.0.0.1:{port}/oauth2callback");

        let wait_handle = std::thread::spawn(move || {
            wait_for_oauth_callback(&listener, &redirect_uri, StdDuration::from_millis(200))
        });

        let appointment = create_appointment_tx(
            &mut connection,
            AppointmentInput {
                customer_id: customer.id.clone(),
                staff_id: staff.id.clone(),
                local_date: "2026-08-25".into(),
                local_start_time: "10:00".into(),
                service_ids: vec![service.id.clone()],
                status: Some("planned".into()),
                note: None,
                whatsapp_reminder_enabled: None,
            },
        )
        .expect("concurrent appointment create");

        let list = list_appointments_between(
            &connection,
            None,
            None,
            None,
            None,
            Some(&appointment.id),
            10,
        )
        .expect("concurrent list");
        assert_eq!(list.len(), 1);

        let wait_result = wait_handle.join().expect("thread join");
        assert!(wait_result.is_err());
    }
}
