use chrono::{Duration, NaiveDate, NaiveTime, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::hash::{Hash, Hasher};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration as StdDuration, Instant};
use tauri::{AppHandle, Manager};
use thiserror::Error;

mod services;

const CORE_SCHEMA_VERSION: i64 = 13;
const ISTANBUL_OFFSET_MINUTES: i64 = 180;
static ID_COUNTER: AtomicU64 = AtomicU64::new(1);

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

#[derive(Debug)]
pub struct AppState {
    database_path: PathBuf,
    sqlite: Mutex<Connection>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LiveServicesConfig {
    google: Option<services::google::GoogleOAuthConfig>,
    supabase: Option<services::supabase::SupabaseConfig>,
}

#[derive(Debug, Clone, Serialize)]
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
    phone: String,
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
    phone: String,
    email: Option<String>,
    notes: Option<String>,
    whatsapp_reminder_enabled: Option<bool>,
    whatsapp_consent_confirmed: Option<bool>,
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
    is_active: Option<bool>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppointmentServiceSnapshot {
    service_id: String,
    service_name_snapshot: String,
    duration_minutes_snapshot: i64,
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
    customer_name: String,
    customer_phone: String,
    staff_name: String,
    service_names: Vec<String>,
    services: Vec<AppointmentServiceSnapshot>,
    updated_at: String,
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

fn open_database(path: &Path) -> Result<Connection, AppError> {
    let connection = Connection::open(path)?;
    connection.pragma_update(None, "journal_mode", "WAL")?;
    connection.pragma_update(None, "foreign_keys", "ON")?;
    migrate_core(&connection)?;
    Ok(connection)
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
          phone TEXT NOT NULL,
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
    connection.execute(
        "UPDATE app_meta SET schema_version = ?1 WHERE schema_version < ?1",
        params![CORE_SCHEMA_VERSION],
    )?;
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

fn create_customer_tx(connection: &Connection, input: CustomerInput) -> Result<Customer, AppError> {
    let first_name = normalize_text(&input.first_name, "firstName", 2, 100)?;
    let last_name = normalize_text(&input.last_name, "lastName", 2, 100)?;
    let phone = normalize_phone(Some(&input.phone), true)?.expect("required phone");
    let email = optional_text(input.email, 254)?;
    let notes = optional_text(input.notes, 2000)?;
    let now = now_iso();
    let consent = input.whatsapp_consent_confirmed.unwrap_or(false);
    let id = new_uuid();
    connection.execute(
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
    )?;
    get_customer(connection, &id)?.ok_or_else(|| AppError::Database("customer insert".to_string()))
}

fn update_customer_tx(
    connection: &Connection,
    id: &str,
    input: CustomerInput,
) -> Result<Customer, AppError> {
    get_customer(connection, id)?
        .ok_or_else(|| AppError::NotFound("CUSTOMER_NOT_FOUND".to_string()))?;
    let first_name = normalize_text(&input.first_name, "firstName", 2, 100)?;
    let last_name = normalize_text(&input.last_name, "lastName", 2, 100)?;
    let phone = normalize_phone(Some(&input.phone), true)?.expect("required phone");
    let email = optional_text(input.email, 254)?;
    let notes = optional_text(input.notes, 2000)?;
    let now = now_iso();
    let consent = input.whatsapp_consent_confirmed.unwrap_or(false);
    connection.execute(
        "UPDATE customers SET first_name=?1,last_name=?2,phone=?3,email=?4,whatsapp_reminder_enabled=?5,whatsapp_consent_confirmed=?6,whatsapp_consent_recorded_at=CASE WHEN ?6 = 1 THEN COALESCE(whatsapp_consent_recorded_at, ?7) ELSE NULL END,notes=?8,updated_at=?7 WHERE id=?9",
        params![first_name,last_name,phone,email,bool_to_i64(input.whatsapp_reminder_enabled.unwrap_or(true)),bool_to_i64(consent),now,notes,id],
    )?;
    get_customer(connection, id)?.ok_or_else(|| AppError::Database("customer update".to_string()))
}

fn create_staff_tx(connection: &Connection, input: StaffInput) -> Result<Staff, AppError> {
    let first_name = normalize_text(&input.first_name, "firstName", 2, 100)?;
    let last_name = optional_text(input.last_name, 100)?;
    let phone = normalize_phone(input.phone.as_deref(), false)?;
    let specialty_note = optional_text(input.specialty_note, 1000)?;
    let color_key = validate_color(&input.color_key)?;
    let now = now_iso();
    let id = new_uuid();
    let sort_order = next_sort_order(connection, "staff", None)?;
    connection.execute(
        "INSERT INTO staff (id, first_name, last_name, phone, specialty_note, color_key, sort_order, is_active, created_at, updated_at)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?9)",
        params![id, first_name, last_name, phone, specialty_note, color_key, sort_order, bool_to_i64(input.is_active.unwrap_or(true)), now],
    )?;
    get_staff(connection, &id)?.ok_or_else(|| AppError::Database("staff insert".to_string()))
}

fn update_staff_tx(
    connection: &Connection,
    id: &str,
    input: StaffInput,
) -> Result<Staff, AppError> {
    get_staff(connection, id)?.ok_or_else(|| AppError::NotFound("STAFF_NOT_FOUND".to_string()))?;
    let first_name = normalize_text(&input.first_name, "firstName", 2, 100)?;
    let last_name = optional_text(input.last_name, 100)?;
    let phone = normalize_phone(input.phone.as_deref(), false)?;
    let specialty_note = optional_text(input.specialty_note, 1000)?;
    let color_key = validate_color(&input.color_key)?;
    connection.execute(
        "UPDATE staff SET first_name=?1,last_name=?2,phone=?3,specialty_note=?4,color_key=?5,is_active=?6,updated_at=?7 WHERE id=?8",
        params![first_name, last_name, phone, specialty_note, color_key, bool_to_i64(input.is_active.unwrap_or(true)), now_iso(), id],
    )?;
    get_staff(connection, id)?.ok_or_else(|| AppError::Database("staff update".to_string()))
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
    connection: &Connection,
    input: ServiceInput,
) -> Result<ServiceItem, AppError> {
    get_category(connection, &input.category_id)
        .map_err(|_| AppError::NotFound("CATEGORY_NOT_FOUND".to_string()))?;
    let name = normalize_text(&input.name, "name", 2, 160)?;
    let duration = input
        .duration_minutes
        .ok_or_else(|| AppError::Validation("duration required".to_string()))?;
    validate_duration(duration)?;
    let id = new_uuid();
    let now = now_iso();
    let sort_order = next_sort_order(
        connection,
        "services",
        Some(("category_id", &input.category_id)),
    )?;
    connection.execute(
        "INSERT INTO services (id, category_id, name, name_key, duration_minutes, sort_order, is_active, created_at, updated_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?8)",
        params![id, input.category_id, name.clone(), name_key(&name), duration, sort_order, bool_to_i64(input.is_active.unwrap_or(true)), now],
    )?;
    get_service(connection, &id)?.ok_or_else(|| AppError::Database("service insert".to_string()))
}

fn validate_duration(duration: i64) -> Result<(), AppError> {
    if !(5..=480).contains(&duration) || duration % 5 != 0 {
        return Err(AppError::Validation("duration invalid".to_string()));
    }
    Ok(())
}

fn update_service_tx(
    connection: &Connection,
    id: &str,
    input: ServiceInput,
) -> Result<ServiceItem, AppError> {
    get_service(connection, id)?
        .ok_or_else(|| AppError::NotFound("SERVICE_NOT_FOUND".to_string()))?;
    get_category(connection, &input.category_id)
        .map_err(|_| AppError::NotFound("CATEGORY_NOT_FOUND".to_string()))?;
    let name = normalize_text(&input.name, "name", 2, 160)?;
    if let Some(duration) = input.duration_minutes {
        validate_duration(duration)?;
    }
    connection.execute(
        "UPDATE services SET category_id=?1,name=?2,name_key=?3,duration_minutes=?4,is_active=?5,updated_at=?6 WHERE id=?7",
        params![input.category_id, name.clone(), name_key(&name), input.duration_minutes, bool_to_i64(input.is_active.unwrap_or(true)), now_iso(), id],
    )?;
    get_service(connection, id)?.ok_or_else(|| AppError::Database("service update".to_string()))
}

fn list_service_items(connection: &Connection) -> Result<Vec<ServiceItem>, AppError> {
    let mut statement = connection.prepare(
        "SELECT s.*, c.name AS category_name, c.is_active AS category_is_active
         FROM services s INNER JOIN service_categories c ON c.id = s.category_id
         ORDER BY c.sort_order ASC, s.sort_order ASC, s.name COLLATE NOCASE ASC",
    )?;
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
    let tx = connection.transaction()?;
    let unique = unique_preserve_order(service_ids);
    for service_id in &unique {
        let service = get_service(&tx, service_id)?
            .ok_or_else(|| AppError::NotFound("SERVICE_NOT_FOUND".to_string()))?;
        if service.availability_status == "inactive"
            || service.availability_status == "category_inactive"
        {
            return Err(AppError::Validation("SERVICE_NOT_ASSIGNABLE".to_string()));
        }
    }
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
    if matches!(status.as_str(), "planned" | "confirmed" | "completed") {
        assert_no_conflict(&tx, &input.staff_id, &start_iso, &end_iso, None)?;
    }
    let id = new_uuid();
    let now = now_iso();
    tx.execute(
        "INSERT INTO appointments (id, customer_id, staff_id, start_at_utc, end_at_utc, total_duration_minutes, status, note, created_at, updated_at)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?9)",
        params![id, input.customer_id, input.staff_id, start_iso, end_iso, total_duration, status, note, now],
    )?;
    for snapshot in &snapshots {
        tx.execute(
            "INSERT INTO appointment_services (appointment_id, service_id, service_name_snapshot, duration_minutes_snapshot, sort_order, created_at)
             VALUES (?1,?2,?3,?4,?5,?6)",
            params![id, snapshot.service_id, snapshot.service_name_snapshot, snapshot.duration_minutes_snapshot, snapshot.sort_order, now],
        )?;
    }
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
    if matches!(status.as_str(), "planned" | "confirmed" | "completed") {
        assert_no_conflict(&tx, &input.staff_id, &start_iso, &end_iso, Some(id))?;
    }
    tx.execute(
        "UPDATE appointments SET customer_id=?1,staff_id=?2,start_at_utc=?3,end_at_utc=?4,total_duration_minutes=?5,status=?6,note=?7,updated_at=?8 WHERE id=?9",
        params![input.customer_id, input.staff_id, start_iso, end_iso, total_duration, status, note, now_iso(), id],
    )?;
    if !used_existing {
        tx.execute(
            "DELETE FROM appointment_services WHERE appointment_id=?1",
            params![id],
        )?;
        let now = now_iso();
        for snapshot in &snapshots {
            tx.execute(
                "INSERT INTO appointment_services (appointment_id, service_id, service_name_snapshot, duration_minutes_snapshot, sort_order, created_at)
                 VALUES (?1,?2,?3,?4,?5,?6)",
                params![id, snapshot.service_id, snapshot.service_name_snapshot, snapshot.duration_minutes_snapshot, snapshot.sort_order, now],
            )?;
        }
    }
    enqueue_google_sync_if_enabled(&tx, id)?;
    reconcile_reminder_for_appointment(&tx, id)?;
    tx.commit()?;
    get_appointment(connection, id)
}

#[derive(Debug)]
struct RawAppointment {
    customer_id: String,
    staff_id: String,
}

fn get_raw_appointment(
    connection: &Connection,
    id: &str,
) -> Result<Option<RawAppointment>, AppError> {
    connection
        .query_row(
            "SELECT customer_id, staff_id FROM appointments WHERE id=?1",
            params![id],
            |row| {
                Ok(RawAppointment {
                    customer_id: row.get(0)?,
                    staff_id: row.get(1)?,
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
        "SELECT service_id, service_name_snapshot, duration_minutes_snapshot, sort_order FROM appointment_services WHERE appointment_id=?1 ORDER BY sort_order ASC",
    )?;
    let rows = statement.query_map(params![appointment_id], |row| {
        Ok(AppointmentServiceSnapshot {
            service_id: row.get(0)?,
            service_name_snapshot: row.get(1)?,
            duration_minutes_snapshot: row.get(2)?,
            sort_order: row.get(3)?,
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

fn backup_destination(database_path: &Path, kind: &str) -> Result<PathBuf, AppError> {
    let backup_dir = database_path
        .parent()
        .and_then(Path::parent)
        .map(|root| root.join("backups"))
        .unwrap_or_else(|| {
            database_path
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .join("backups")
        });
    fs::create_dir_all(&backup_dir)?;
    Ok(backup_dir.join(format!("salon-{kind}-{}.db", timestamp_for_file())))
}

fn sanitize_machine_bound_state(database_path: &Path) -> Result<bool, AppError> {
    let connection = Connection::open(database_path)?;
    connection.pragma_update(None, "foreign_keys", "ON")?;
    connection.execute("DELETE FROM secure_secrets", [])?;
    connection.execute(
        "UPDATE google_calendar_settings SET sync_enabled=0, account_email=NULL, updated_at_utc=?1 WHERE id=1",
        params![now_iso()],
    )?;
    connection.execute(
        "UPDATE cloud_reminder_settings SET cloud_mode_enabled=0, updated_at_utc=?1 WHERE id=1",
        params![now_iso()],
    )?;
    connection.execute(
        "UPDATE whatsapp_settings SET is_enabled=0, automatic_reminder_enabled=0, updated_at=?1 WHERE id=1",
        params![now_iso()],
    )?;
    if !integrity_check(&connection)? {
        return Err(AppError::Database("BACKUP_INTEGRITY_FAILED".to_string()));
    }
    Ok(true)
}

fn create_sanitized_backup(
    connection: &Connection,
    database_path: &Path,
    kind: &str,
) -> Result<DatabaseBackupSummary, AppError> {
    if !integrity_check(connection)? {
        return Err(AppError::Database("SOURCE_INTEGRITY_FAILED".to_string()));
    }
    let backup_path = backup_destination(database_path, kind)?;
    let sql = format!("VACUUM INTO '{}';", sql_path_literal(&backup_path));
    connection.execute_batch(&sql)?;
    let removed = sanitize_machine_bound_state(&backup_path)?;
    Ok(DatabaseBackupSummary {
        database_path: database_path.display().to_string(),
        backup_path: backup_path.display().to_string(),
        integrity: "ok".to_string(),
        machine_bound_secrets_removed: removed,
        created_at_utc: now_iso(),
    })
}

fn clean_database_in_place(
    connection: &mut Connection,
    database_path: &Path,
) -> Result<DatabaseCleanSummary, AppError> {
    let safety = create_sanitized_backup(connection, database_path, "pre-clean")?;
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
    payload_json: Option<String>,
) -> Result<(), AppError> {
    let now = now_iso();
    connection.execute(
        "INSERT INTO reminder_cloud_state (reminder_id, last_synced_revision, next_revision, created_at_utc, updated_at_utc)
         VALUES (?1, 0, 1, ?2, ?2)
         ON CONFLICT(reminder_id) DO NOTHING",
        params![reminder_id, now],
    )?;
    let revision: i64 = connection.query_row(
        "SELECT next_revision FROM reminder_cloud_state WHERE reminder_id=?1",
        params![reminder_id],
        |row| row.get(0),
    )?;
    let mutation_id = format!("{reminder_id}:{revision}:{action}");
    let payload_hash = pseudo_hash_64(&format!("{mutation_id}:{:?}", payload_json));
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
            "SELECT a.start_at_utc, a.status, c.is_active, c.phone, c.whatsapp_reminder_enabled, c.whatsapp_consent_confirmed
             FROM appointments a INNER JOIN customers c ON c.id=a.customer_id WHERE a.id=?1",
            params![appointment_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, i64>(5)?,
                ))
            },
        )
        .optional()?;
    let Some((start_at, status, active, phone, reminder_enabled, consent)) = row else {
        return Ok(false);
    };
    let eligible = matches!(status.as_str(), "planned" | "confirmed")
        && active == 1
        && reminder_enabled == 1
        && consent == 1
        && normalize_phone(Some(&phone), true).is_ok();
    let existing: Option<String> = connection
        .query_row(
            "SELECT id FROM appointment_reminders WHERE appointment_id=?1 AND channel='whatsapp' AND reminder_type='appointment_24h'",
            params![appointment_id],
            |row| row.get(0),
        )
        .optional()?;
    let now = now_iso();
    if eligible {
        let scheduled = chrono::DateTime::parse_from_rfc3339(&start_at)
            .map_err(|_| AppError::Validation("appointment start_at invalid".to_string()))?
            .with_timezone(&Utc)
            - Duration::hours(24);
        let scheduled = utc_iso(scheduled);
        let reminder_id = existing.unwrap_or_else(new_uuid);
        connection.execute(
            "INSERT INTO appointment_reminders (id, appointment_id, scheduled_for_utc, status, created_at, updated_at)
             VALUES (?1, ?2, ?3, 'pending', ?4, ?4)
             ON CONFLICT(appointment_id, channel, reminder_type)
             DO UPDATE SET scheduled_for_utc=excluded.scheduled_for_utc, status='pending', cancelled_at_utc=NULL, updated_at=excluded.updated_at",
            params![reminder_id, appointment_id, scheduled, now],
        )?;
        upsert_cloud_outbox(
            connection,
            &reminder_id,
            "upsert",
            Some(format!(
                r#"{{"appointmentId":"{appointment_id}","scheduledForUtc":"{scheduled}"}}"#
            )),
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
    sql.push_str(" ORDER BY a.start_at_utc ASC LIMIT :limit");
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
            row.get::<_, String>("customer_name")?,
            row.get::<_, String>("customer_phone")?,
            row.get::<_, String>("staff_name")?,
            row.get::<_, String>("updated_at")?,
        ))
    })?;
    let mut output = Vec::new();
    for row in rows {
        let (
            id,
            customer_id,
            staff_id,
            start_at_utc,
            end_at_utc,
            total_duration_minutes,
            status,
            note,
            customer_name,
            customer_phone,
            staff_name,
            updated_at,
        ) = row?;
        let services = list_appointment_services(connection, &id)?;
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
fn customer_create(
    input: CustomerInput,
    state: tauri::State<'_, AppState>,
) -> Result<Customer, AppError> {
    let connection = state.sqlite.lock().expect("sqlite mutex poisoned");
    create_customer_tx(&connection, input)
}

#[tauri::command]
fn customer_update(
    id: String,
    input: CustomerInput,
    state: tauri::State<'_, AppState>,
) -> Result<Customer, AppError> {
    let connection = state.sqlite.lock().expect("sqlite mutex poisoned");
    update_customer_tx(&connection, &id, input)
}

#[tauri::command]
fn customer_set_active(
    id: String,
    is_active: bool,
    state: tauri::State<'_, AppState>,
) -> Result<Customer, AppError> {
    let connection = state.sqlite.lock().expect("sqlite mutex poisoned");
    connection.execute(
        "UPDATE customers SET is_active=?1, updated_at=?2 WHERE id=?3",
        params![bool_to_i64(is_active), now_iso(), id],
    )?;
    get_customer(&connection, &id)?
        .ok_or_else(|| AppError::NotFound("CUSTOMER_NOT_FOUND".to_string()))
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
    let status_sql = match status.as_deref().unwrap_or("active") {
        "inactive" => "is_active = 0",
        "all" => "1 = 1",
        _ => "is_active = 1",
    };
    let query = search.unwrap_or_default().to_lowercase();
    let phone_query: String = query.chars().filter(|c| c.is_ascii_digit()).collect();
    let mut statement = connection.prepare(&format!(
        "SELECT * FROM customers WHERE {status_sql}
         AND (:query = '' OR lower(first_name) LIKE :like OR lower(last_name) LIKE :like OR lower(first_name || ' ' || last_name) LIKE :like OR phone LIKE :phone)
         ORDER BY last_name COLLATE NOCASE ASC, first_name COLLATE NOCASE ASC LIMIT :limit"
    ))?;
    let like = format!("%{query}%");
    let phone = format!(
        "%{}%",
        phone_query.trim_start_matches("90").trim_start_matches('0')
    );
    let rows = statement.query_map(
        &[
            (":query", &query as &dyn rusqlite::ToSql),
            (":like", &like),
            (":phone", &phone),
            (":limit", &limit),
        ],
        customer_from_row,
    )?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

#[tauri::command]
fn staff_create(input: StaffInput, state: tauri::State<'_, AppState>) -> Result<Staff, AppError> {
    let connection = state.sqlite.lock().expect("sqlite mutex poisoned");
    create_staff_tx(&connection, input)
}

#[tauri::command]
fn staff_update(
    id: String,
    input: StaffInput,
    state: tauri::State<'_, AppState>,
) -> Result<Staff, AppError> {
    let connection = state.sqlite.lock().expect("sqlite mutex poisoned");
    update_staff_tx(&connection, &id, input)
}

#[tauri::command]
fn staff_set_active(
    id: String,
    is_active: bool,
    state: tauri::State<'_, AppState>,
) -> Result<Staff, AppError> {
    let connection = state.sqlite.lock().expect("sqlite mutex poisoned");
    connection.execute(
        "UPDATE staff SET is_active=?1, updated_at=?2 WHERE id=?3",
        params![bool_to_i64(is_active), now_iso(), id],
    )?;
    get_staff(&connection, &id)?.ok_or_else(|| AppError::NotFound("STAFF_NOT_FOUND".to_string()))
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
    let mut connection = state.sqlite.lock().expect("sqlite mutex poisoned");
    set_staff_services_tx(&mut connection, &staff_id, service_ids)
}

#[tauri::command]
fn service_category_create(
    input: CategoryInput,
    state: tauri::State<'_, AppState>,
) -> Result<ServiceCategory, AppError> {
    let connection = state.sqlite.lock().expect("sqlite mutex poisoned");
    create_category_tx(&connection, input)
}

#[tauri::command]
fn service_create(
    input: ServiceInput,
    state: tauri::State<'_, AppState>,
) -> Result<ServiceItem, AppError> {
    let connection = state.sqlite.lock().expect("sqlite mutex poisoned");
    create_service_tx(&connection, input)
}

#[tauri::command]
fn service_update(
    id: String,
    input: ServiceInput,
    state: tauri::State<'_, AppState>,
) -> Result<ServiceItem, AppError> {
    let connection = state.sqlite.lock().expect("sqlite mutex poisoned");
    update_service_tx(&connection, &id, input)
}

#[tauri::command]
fn service_set_active(
    id: String,
    is_active: bool,
    state: tauri::State<'_, AppState>,
) -> Result<ServiceItem, AppError> {
    let connection = state.sqlite.lock().expect("sqlite mutex poisoned");
    connection.execute(
        "UPDATE services SET is_active=?1, updated_at=?2 WHERE id=?3",
        params![bool_to_i64(is_active), now_iso(), id],
    )?;
    get_service(&connection, &id)?
        .ok_or_else(|| AppError::NotFound("SERVICE_NOT_FOUND".to_string()))
}

#[tauri::command]
fn service_list(state: tauri::State<'_, AppState>) -> Result<Vec<ServiceItem>, AppError> {
    let connection = state.sqlite.lock().expect("sqlite mutex poisoned");
    list_service_items(&connection)
}

#[tauri::command]
fn appointment_create(
    input: AppointmentInput,
    state: tauri::State<'_, AppState>,
) -> Result<AppointmentSummary, AppError> {
    let mut connection = state.sqlite.lock().expect("sqlite mutex poisoned");
    create_appointment_tx(&mut connection, input)
}

#[tauri::command]
fn appointment_update(
    id: String,
    input: AppointmentInput,
    state: tauri::State<'_, AppState>,
) -> Result<AppointmentSummary, AppError> {
    let mut connection = state.sqlite.lock().expect("sqlite mutex poisoned");
    update_appointment_tx(&mut connection, &id, input)
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
fn database_create_backup(
    state: tauri::State<'_, AppState>,
) -> Result<DatabaseBackupSummary, AppError> {
    let connection = state.sqlite.lock().expect("sqlite mutex poisoned");
    create_sanitized_backup(&connection, &state.database_path, "manual")
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
    let mut connection = state.sqlite.lock().expect("sqlite mutex poisoned");
    clean_database_in_place(&mut connection, &state.database_path)
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
    let mut config = read_live_services_config_file()?.unwrap_or_default();

    let google_client_id = std::env::var("BEAUTYSALOON_GOOGLE_CLIENT_ID")
        .ok()
        .filter(|value| !value.trim().is_empty());
    let google_client_secret = std::env::var("BEAUTYSALOON_GOOGLE_CLIENT_SECRET")
        .ok()
        .filter(|value| !value.trim().is_empty());
    if let (Some(client_id), Some(client_secret)) = (google_client_id, google_client_secret) {
        config.google = Some(services::google::GoogleOAuthConfig {
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

fn read_live_services_config_file() -> Result<Option<LiveServicesConfig>, AppError> {
    for path in live_services_config_candidates()? {
        if path.exists() {
            let text = fs::read_to_string(path)?;
            return parse_live_services_config(&text).map(Some);
        }
    }
    Ok(None)
}

fn live_services_config_candidates() -> Result<Vec<PathBuf>, AppError> {
    let mut candidates = Vec::new();
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

fn parse_live_services_config(text: &str) -> Result<LiveServicesConfig, AppError> {
    serde_json::from_str(text.trim_start_matches('\u{feff}'))
        .map_err(|_| AppError::Validation("LIVE_SERVICE_CONFIG_INVALID".to_string()))
}

fn google_config() -> Result<services::google::GoogleOAuthConfig, AppError> {
    load_live_services_config()?
        .google
        .ok_or_else(|| AppError::Validation("GOOGLE_CONFIG_MISSING".to_string()))
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
    let _guard = try_acquire_google_connect_guard()?;
    let config = google_config()?;
    let database_path = state.database_path.clone();

    tauri::async_runtime::spawn_blocking(move || {
        let _guard = _guard;
        let listener = TcpListener::bind("127.0.0.1:0")?;
        listener.set_nonblocking(true)?;
        let redirect_uri = format!(
            "http://127.0.0.1:{}/oauth2callback",
            listener.local_addr()?.port()
        );
        let state_token = new_uuid();
        let auth_url =
            services::google::build_google_auth_url(&config.client_id, &redirect_uri, &state_token)?;
        open_system_browser(&auth_url)?;
        let callback_url =
            wait_for_oauth_callback(&listener, &redirect_uri, StdDuration::from_secs(120))?;
        let code = services::google::parse_oauth_callback(&callback_url, &state_token)?;
        let mut transport = services::google::ReqwestHttpTransport;
        let token_set =
            services::google::exchange_auth_code(&mut transport, &config, &redirect_uri, &code)?;
        let calendar_id = config
            .calendar_id
            .clone()
            .unwrap_or_else(|| "primary".to_string());
        let connection = open_database(&database_path)?;
        let protector = services::secure_store::WindowsDpapiProtector;
        services::secure_store::upsert_secret(
            &connection,
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
        connection.execute(
            "UPDATE google_calendar_outbox SET sync_status='in_flight', last_attempt_at_utc=strftime('%Y-%m-%dT%H:%M:%fZ','now'), last_error_code=NULL WHERE appointment_id=?1 AND sync_status='pending'",
            params![appointment_id],
        )?;
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
                processed += 1;
            }
            Err(error) => {
                let code = error.to_string();
                connection.execute(
                    "UPDATE google_calendar_outbox SET sync_status='pending', last_error_code=?1, updated_at_utc=strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE appointment_id=?2",
                    params![code, appointment_id],
                )?;
                break;
            }
        }
    }
    Ok(processed)
}

fn open_system_browser(url: &str) -> Result<(), AppError> {
    if !url.starts_with("https://") && !url.starts_with("http://") {
        return Err(AppError::Validation("INVALID_AUTH_URL_SCHEME".to_string()));
    }
    #[cfg(windows)]
    {
        let status = Command::new("rundll32.exe")
            .args(["url.dll,FileProtocolHandler", url])
            .status()?;
        if status.success() {
            return Ok(());
        }
    }
    #[cfg(not(windows))]
    {
        let status = Command::new("xdg-open").arg(url).status()?;
        if status.success() {
            return Ok(());
        }
    }
    Err(AppError::Database("GOOGLE_BROWSER_OPEN_FAILED".to_string()))
}

fn wait_for_oauth_callback(
    listener: &TcpListener,
    redirect_uri: &str,
    timeout: StdDuration,
) -> Result<String, AppError> {
    let started = Instant::now();
    while started.elapsed() < timeout {
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
                    let response = "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
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

#[tauri::command]
fn cloud_connection_status(
    state: tauri::State<'_, AppState>,
) -> Result<services::supabase::SupabaseConnectionStatus, AppError> {
    let connection = state.sqlite.lock().expect("sqlite mutex poisoned");
    Ok(services::supabase::SupabaseConnectionStatus {
        configured: load_live_services_config()?.supabase.is_some(),
        session_present: services::reminder_cloud::has_session_secret(&connection)?,
    })
}

#[tauri::command]
fn cloud_status(
    state: tauri::State<'_, AppState>,
) -> Result<services::supabase::SupabaseConnectionStatus, AppError> {
    cloud_connection_status(state)
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
    tauri::Builder::default()
        .setup(|app| {
            let root = app_data_root(app.handle())?;
            let (database_path, _, _, _, _) = data_paths(&root)?;
            let sqlite = open_database(&database_path)?;
            app.manage(AppState {
                database_path,
                sqlite: Mutex::new(sqlite),
            });
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.show();
                let _ = window.set_focus();
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            app_health,
            customer_create,
            customer_update,
            customer_set_active,
            customer_search,
            staff_create,
            staff_update,
            staff_set_active,
            staff_list,
            staff_set_services,
            service_category_create,
            service_create,
            service_update,
            service_set_active,
            service_list,
            appointment_create,
            appointment_update,
            appointment_list_by_date,
            database_create_backup,
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
            cloud_status,
            cloud_request_otp,
            otp_request,
            cloud_verify_otp,
            otp_verify,
            cloud_disconnect,
            cloud_process_outbox
        ])
        .run(tauri::generate_context!())
        .expect("error while running BeautySaloon");
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn open_temp() -> (tempfile::TempDir, Connection) {
        let temp = tempdir().expect("tempdir");
        let connection = open_database(&temp.path().join("core.db")).expect("open db");
        (temp, connection)
    }

    fn seed_core(connection: &mut Connection) -> (Customer, Staff, ServiceItem) {
        let customer = create_customer_tx(
            connection,
            CustomerInput {
                first_name: "Ayse".into(),
                last_name: "Yilmaz".into(),
                phone: "+90 555 111 22 33".into(),
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
                is_active: Some(true),
            },
        )
        .expect("service");
        set_staff_services_tx(connection, &staff.id, vec![service.id.clone()]).expect("assign");
        (customer, staff, service)
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
    fn customer_staff_service_crud_and_phone_rules() {
        let (_temp, mut connection) = open_temp();
        let (customer, staff, service) = seed_core(&mut connection);
        assert_eq!(customer.phone, "5551112233");
        assert_eq!(staff.phone.as_deref(), Some("5552223344"));
        assert_eq!(service.availability_status, "ready");

        let updated = update_customer_tx(
            &connection,
            &customer.id,
            CustomerInput {
                first_name: "Ayse Nur".into(),
                last_name: "Yilmaz".into(),
                phone: "05551112233".into(),
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
        assert_eq!(list_service_items(&connection).expect("services").len(), 1);
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
        let status_sql = if status == "all" {
            "1 = 1"
        } else {
            "is_active = 1"
        };
        let like = format!("%{}%", search.to_lowercase());
        let mut statement = connection.prepare(&format!(
            "SELECT * FROM customers WHERE {status_sql} AND (lower(first_name) LIKE ?1 OR lower(last_name) LIKE ?1 OR phone LIKE ?1)"
        ))?;
        let rows = statement.query_map(params![like], customer_from_row)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
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

    fn future_local_slot(hours_from_now: i64) -> (String, String) {
        let local = Utc::now()
            + Duration::minutes(ISTANBUL_OFFSET_MINUTES)
            + Duration::hours(hours_from_now);
        let minute = (chrono::Timelike::minute(&local) / 5) * 5;
        (
            local.date_naive().format("%Y-%m-%d").to_string(),
            format!("{:02}:{:02}", chrono::Timelike::hour(&local), minute),
        )
    }

    #[test]
    fn backup_clean_restore_and_corrupt_guard_sanitize_machine_bound_state() {
        let temp = tempdir().expect("tempdir");
        let db_path = temp.path().join("database").join("salon-foundation.db");
        fs::create_dir_all(db_path.parent().expect("db parent")).expect("db dir");
        let mut connection = open_database(&db_path).expect("open");
        let (customer, _staff, _service) = seed_core(&mut connection);
        connection
            .execute(
                "INSERT INTO secure_secrets (secret_key, encrypted_value, encryption_provider, created_at, updated_at)
                 VALUES ('google_calendar_refresh_token', x'0102', 'electron_safe_storage_v1', ?1, ?1)",
                params![now_iso()],
            )
            .expect("secret");
        connection
            .execute("UPDATE google_calendar_settings SET sync_enabled=1, account_email='owner@example.test' WHERE id=1", [])
            .expect("google on");

        let backup = create_sanitized_backup(&connection, &db_path, "manual").expect("backup");
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
        assert_eq!(secret_count, 0);
        assert_eq!(google_enabled, 0);

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
            &connection,
            &customer.id,
            CustomerInput {
                first_name: customer.first_name.clone(),
                last_name: customer.last_name.clone(),
                phone: "05551112233".into(),
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
        assert!(persisted >= 2);
        assert!(integrity_check(&reopened).expect("integrity"));
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
        )
        .expect("auth url");
        assert!(
            auth_url.contains("scope=https%3A%2F%2Fwww.googleapis.com%2Fauth%2Fcalendar.events")
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
            &connection,
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
            &connection,
            &customer.id,
            CustomerInput {
                first_name: customer.first_name,
                last_name: customer.last_name,
                phone: "05551112233".into(),
                email: None,
                notes: None,
                whatsapp_reminder_enabled: Some(true),
                whatsapp_consent_confirmed: Some(true),
            },
        )
        .expect("customer");
        create_appointment_tx(
            &mut connection,
            AppointmentInput {
                customer_id: customer.id,
                staff_id: staff.id,
                local_date: "2026-08-26".into(),
                local_start_time: "10:00".into(),
                service_ids: vec![service.id],
                status: Some("confirmed".into()),
                note: None,
            },
        )
        .expect("appointment");
        reconcile_all_reminders_mock(&connection).expect("project");
        let reminder_id: String = connection
            .query_row("SELECT id FROM appointment_reminders LIMIT 1", [], |row| {
                row.get(0)
            })
            .expect("reminder");
        upsert_cloud_outbox(&connection, &reminder_id, "cancel", None).expect("second revision");

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
            services::google::HttpResponse { status: 200, body: br#"{"data":{"revision":3,"status":"cancelled","remoteUpdatedAtUtc":"2026-08-25T00:02:00.000Z"}}"#.to_vec() },
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
        let second = services::reminder_cloud::process_ordered_outbox(
            &connection,
            &mut transport,
            &config,
            &session,
            10,
        )
        .expect("second sync");
        assert_eq!(second.processed, 1);
        let third = services::reminder_cloud::process_ordered_outbox(
            &connection,
            &mut transport,
            &config,
            &session,
            10,
        )
        .expect("third sync");
        assert_eq!(third.processed, 1);
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
            3,
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

        let callback_url = wait_for_oauth_callback(&listener, &redirect_uri, StdDuration::from_secs(5))
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
    fn duplicate_connect_prevented_by_guard() {
        let guard1 = try_acquire_google_connect_guard().expect("first acquire");
        let guard2_err = try_acquire_google_connect_guard().expect_err("second acquire should fail");
        assert!(matches!(guard2_err, AppError::Conflict(_)));
        drop(guard1);
        let guard3 = try_acquire_google_connect_guard().expect("third acquire after drop");
        drop(guard3);
    }

    #[test]
    fn oauth_callback_timeout_returns_safe_error() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        listener.set_nonblocking(true).expect("nonblocking");
        let port = listener.local_addr().expect("port").port();
        let redirect_uri = format!("http://127.0.0.1:{port}/oauth2callback");

        let result = wait_for_oauth_callback(&listener, &redirect_uri, StdDuration::from_millis(50));
        assert!(matches!(result, Err(AppError::Validation(msg)) if msg == "GOOGLE_OAUTH_TIMEOUT"));
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
            },
        )
        .expect("concurrent appointment create");

        let list = list_appointments_between(&connection, None, None, None, None, Some(&appointment.id), 10)
            .expect("concurrent list");
        assert_eq!(list.len(), 1);

        let wait_result = wait_handle.join().expect("thread join");
        assert!(wait_result.is_err());
    }
}



