use chrono::{Duration, NaiveDate, NaiveTime, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use tauri::{AppHandle, Manager};
use thiserror::Error;

const CORE_SCHEMA_VERSION: i64 = 7;
const ISTANBUL_OFFSET_MINUTES: i64 = 180;
static ID_COUNTER: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Error)]
enum AppError {
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

    for dir in [&database_dir, &backups_dir, &logs_dir, &exports_dir, &settings_dir] {
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
        .query_row("SELECT MAX(schema_version) FROM app_meta", [], |row| row.get(0))
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
        return if required { Err(AppError::Validation("phone required".to_string())) } else { Ok(None) };
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return if required { Err(AppError::Validation("phone required".to_string())) } else { Ok(None) };
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
    name.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

fn bool_to_i64(value: bool) -> i64 {
    if value { 1 } else { 0 }
}

fn validate_color(value: &str) -> Result<String, AppError> {
    match value {
        "sage" | "teal" | "blue" | "purple" | "rose" | "coral" | "amber" | "slate" => Ok(value.to_string()),
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
    let date = NaiveDate::parse_from_str(local_date, "%Y-%m-%d").map_err(|_| AppError::Validation("localDate invalid".to_string()))?;
    let time = NaiveTime::parse_from_str(local_time, "%H:%M").map_err(|_| AppError::Validation("localStartTime invalid".to_string()))?;
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

fn ensure_same_istanbul_day(start: chrono::DateTime<Utc>, end: chrono::DateTime<Utc>) -> Result<(), AppError> {
    let start_day = (start + Duration::minutes(ISTANBUL_OFFSET_MINUTES)).date_naive();
    let end_day = (end - Duration::milliseconds(1) + Duration::minutes(ISTANBUL_OFFSET_MINUTES)).date_naive();
    if start_day != end_day {
        return Err(AppError::Validation("APPOINTMENT_CROSSES_MIDNIGHT".to_string()));
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
        .query_row("SELECT * FROM customers WHERE id = ?1", params![id], customer_from_row)
        .optional()
        .map_err(Into::into)
}

fn get_staff(connection: &Connection, id: &str) -> Result<Option<Staff>, AppError> {
    connection
        .query_row("SELECT * FROM staff WHERE id = ?1", params![id], staff_from_row)
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

fn next_sort_order(connection: &Connection, table: &str, where_sql: Option<(&str, &str)>) -> Result<i64, AppError> {
    let sql = match where_sql {
        Some((column, _)) => format!("SELECT COALESCE(MAX(sort_order), 0) + 10 FROM {table} WHERE {column} = ?1"),
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

fn update_customer_tx(connection: &Connection, id: &str, input: CustomerInput) -> Result<Customer, AppError> {
    get_customer(connection, id)?.ok_or_else(|| AppError::NotFound("CUSTOMER_NOT_FOUND".to_string()))?;
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

fn update_staff_tx(connection: &Connection, id: &str, input: StaffInput) -> Result<Staff, AppError> {
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

fn create_category_tx(connection: &Connection, input: CategoryInput) -> Result<ServiceCategory, AppError> {
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

fn create_service_tx(connection: &Connection, input: ServiceInput) -> Result<ServiceItem, AppError> {
    get_category(connection, &input.category_id).map_err(|_| AppError::NotFound("CATEGORY_NOT_FOUND".to_string()))?;
    let name = normalize_text(&input.name, "name", 2, 160)?;
    let duration = input.duration_minutes.ok_or_else(|| AppError::Validation("duration required".to_string()))?;
    validate_duration(duration)?;
    let id = new_uuid();
    let now = now_iso();
    let sort_order = next_sort_order(connection, "services", Some(("category_id", &input.category_id)))?;
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

fn update_service_tx(connection: &Connection, id: &str, input: ServiceInput) -> Result<ServiceItem, AppError> {
    get_service(connection, id)?.ok_or_else(|| AppError::NotFound("SERVICE_NOT_FOUND".to_string()))?;
    get_category(connection, &input.category_id).map_err(|_| AppError::NotFound("CATEGORY_NOT_FOUND".to_string()))?;
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

fn set_staff_services_tx(connection: &mut Connection, staff_id: &str, service_ids: Vec<String>) -> Result<Vec<String>, AppError> {
    get_staff(connection, staff_id)?.ok_or_else(|| AppError::NotFound("STAFF_NOT_FOUND".to_string()))?;
    let tx = connection.transaction()?;
    let unique = unique_preserve_order(service_ids);
    for service_id in &unique {
        let service = get_service(&tx, service_id)?.ok_or_else(|| AppError::NotFound("SERVICE_NOT_FOUND".to_string()))?;
        if service.availability_status == "inactive" || service.availability_status == "category_inactive" {
            return Err(AppError::Validation("SERVICE_NOT_ASSIGNABLE".to_string()));
        }
    }
    tx.execute("DELETE FROM staff_services WHERE staff_id = ?1", params![staff_id])?;
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
        let existing_ids: Vec<String> = existing.iter().map(|item| item.service_id.clone()).collect();
        if existing_ids == service_ids {
            let total = existing.iter().map(|item| item.duration_minutes_snapshot).sum();
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
            return Err(AppError::Validation("SERVICE_NOT_ASSIGNED_TO_STAFF".to_string()));
        }
        let service = get_service(connection, service_id)?.ok_or_else(|| AppError::NotFound("SERVICE_NOT_FOUND".to_string()))?;
        if service.availability_status != "ready" {
            return Err(AppError::Validation("SERVICE_NOT_READY".to_string()));
        }
        let duration = service.duration_minutes.ok_or_else(|| AppError::Validation("SERVICE_NOT_READY".to_string()))?;
        snapshots.push(AppointmentServiceSnapshot {
            service_id: service.id,
            service_name_snapshot: service.name,
            duration_minutes_snapshot: duration,
            sort_order: ((index + 1) * 10) as i64,
        });
    }
    let total: i64 = snapshots.iter().map(|item| item.duration_minutes_snapshot).sum();
    if !(5..=720).contains(&total) {
        return Err(AppError::Validation("duration total".to_string()));
    }
    Ok((snapshots, total, false))
}

fn assert_appointment_references(connection: &Connection, customer_id: &str, staff_id: &str, current: Option<(&str, &str)>) -> Result<(), AppError> {
    let customer = get_customer(connection, customer_id)?.ok_or_else(|| AppError::NotFound("CUSTOMER_NOT_FOUND".to_string()))?;
    if !customer.is_active && current.map(|(cid, _)| cid != customer_id).unwrap_or(true) {
        return Err(AppError::Validation("CUSTOMER_INACTIVE".to_string()));
    }
    let staff = get_staff(connection, staff_id)?.ok_or_else(|| AppError::NotFound("STAFF_NOT_FOUND".to_string()))?;
    if !staff.is_active && current.map(|(_, sid)| sid != staff_id).unwrap_or(true) {
        return Err(AppError::Validation("STAFF_INACTIVE".to_string()));
    }
    Ok(())
}

fn assert_no_conflict(connection: &Connection, staff_id: &str, start_at: &str, end_at: &str, exclude_id: Option<&str>) -> Result<(), AppError> {
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

fn create_appointment_tx(connection: &mut Connection, input: AppointmentInput) -> Result<AppointmentSummary, AppError> {
    let status = validate_status(input.status.as_deref().unwrap_or("planned"))?;
    let note = optional_text(input.note, 2000)?;
    let tx = connection.transaction()?;
    assert_appointment_references(&tx, &input.customer_id, &input.staff_id, None)?;
    let (snapshots, total_duration, _) = appointment_snapshots(&tx, &input.staff_id, input.service_ids, None)?;
    let start = local_to_utc(&input.local_date, &input.local_start_time)?;
    let end = start + Duration::minutes(total_duration);
    ensure_same_istanbul_day(start, end)?;
    let start_iso = utc_iso(start);
    let end_iso = utc_iso(end);
    if status == "no_show" && start > Utc::now() {
        return Err(AppError::Validation("APPOINTMENT_NO_SHOW_TOO_EARLY".to_string()));
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
    tx.commit()?;
    get_appointment(connection, &id)
}

fn update_appointment_tx(connection: &mut Connection, id: &str, input: AppointmentInput) -> Result<AppointmentSummary, AppError> {
    let status = validate_status(input.status.as_deref().unwrap_or("planned"))?;
    let note = optional_text(input.note, 2000)?;
    let current = get_raw_appointment(connection, id)?.ok_or_else(|| AppError::NotFound("APPOINTMENT_NOT_FOUND".to_string()))?;
    let tx = connection.transaction()?;
    assert_appointment_references(&tx, &input.customer_id, &input.staff_id, Some((&current.customer_id, &current.staff_id)))?;
    let existing = if current.staff_id == input.staff_id {
        Some(list_appointment_services(&tx, id)?)
    } else {
        None
    };
    let (snapshots, total_duration, used_existing) = appointment_snapshots(&tx, &input.staff_id, input.service_ids, existing)?;
    let start = local_to_utc(&input.local_date, &input.local_start_time)?;
    let end = start + Duration::minutes(total_duration);
    ensure_same_istanbul_day(start, end)?;
    let start_iso = utc_iso(start);
    let end_iso = utc_iso(end);
    if status == "no_show" && start > Utc::now() {
        return Err(AppError::Validation("APPOINTMENT_NO_SHOW_TOO_EARLY".to_string()));
    }
    if matches!(status.as_str(), "planned" | "confirmed" | "completed") {
        assert_no_conflict(&tx, &input.staff_id, &start_iso, &end_iso, Some(id))?;
    }
    tx.execute(
        "UPDATE appointments SET customer_id=?1,staff_id=?2,start_at_utc=?3,end_at_utc=?4,total_duration_minutes=?5,status=?6,note=?7,updated_at=?8 WHERE id=?9",
        params![input.customer_id, input.staff_id, start_iso, end_iso, total_duration, status, note, now_iso(), id],
    )?;
    if !used_existing {
        tx.execute("DELETE FROM appointment_services WHERE appointment_id=?1", params![id])?;
        let now = now_iso();
        for snapshot in &snapshots {
            tx.execute(
                "INSERT INTO appointment_services (appointment_id, service_id, service_name_snapshot, duration_minutes_snapshot, sort_order, created_at)
                 VALUES (?1,?2,?3,?4,?5,?6)",
                params![id, snapshot.service_id, snapshot.service_name_snapshot, snapshot.duration_minutes_snapshot, snapshot.sort_order, now],
            )?;
        }
    }
    tx.commit()?;
    get_appointment(connection, id)
}

#[derive(Debug)]
struct RawAppointment {
    customer_id: String,
    staff_id: String,
}

fn get_raw_appointment(connection: &Connection, id: &str) -> Result<Option<RawAppointment>, AppError> {
    connection
        .query_row(
            "SELECT customer_id, staff_id FROM appointments WHERE id=?1",
            params![id],
            |row| Ok(RawAppointment { customer_id: row.get(0)?, staff_id: row.get(1)? }),
        )
        .optional()
        .map_err(Into::into)
}

fn list_appointment_services(connection: &Connection, appointment_id: &str) -> Result<Vec<AppointmentServiceSnapshot>, AppError> {
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
    let mut appointments = list_appointments_between(connection, None, None, None, None, Some(id), 1)?;
    appointments.pop().ok_or_else(|| AppError::NotFound("APPOINTMENT_NOT_FOUND".to_string()))
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
    let named: Vec<(&str, &dyn rusqlite::ToSql)> = params_map.iter().map(|(key, value)| (*key, value as &dyn rusqlite::ToSql)).collect();
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
        ))
    })?;
    let mut output = Vec::new();
    for row in rows {
        let (id, customer_id, staff_id, start_at_utc, end_at_utc, total_duration_minutes, status, note, customer_name, customer_phone, staff_name) = row?;
        let services = list_appointment_services(connection, &id)?;
        let service_names = services.iter().map(|item| item.service_name_snapshot.clone()).collect();
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
        message: if ok { "SQLite core data layer hazir.".to_string() } else { "SQLite integrity check basarisiz.".to_string() },
    })
}

#[tauri::command]
fn customer_create(input: CustomerInput, state: tauri::State<'_, AppState>) -> Result<Customer, AppError> {
    let connection = state.sqlite.lock().expect("sqlite mutex poisoned");
    create_customer_tx(&connection, input)
}

#[tauri::command]
fn customer_update(id: String, input: CustomerInput, state: tauri::State<'_, AppState>) -> Result<Customer, AppError> {
    let connection = state.sqlite.lock().expect("sqlite mutex poisoned");
    update_customer_tx(&connection, &id, input)
}

#[tauri::command]
fn customer_set_active(id: String, is_active: bool, state: tauri::State<'_, AppState>) -> Result<Customer, AppError> {
    let connection = state.sqlite.lock().expect("sqlite mutex poisoned");
    connection.execute("UPDATE customers SET is_active=?1, updated_at=?2 WHERE id=?3", params![bool_to_i64(is_active), now_iso(), id])?;
    get_customer(&connection, &id)?.ok_or_else(|| AppError::NotFound("CUSTOMER_NOT_FOUND".to_string()))
}

#[tauri::command]
fn customer_search(search: Option<String>, status: Option<String>, limit: Option<i64>, state: tauri::State<'_, AppState>) -> Result<Vec<Customer>, AppError> {
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
    let phone = format!("%{}%", phone_query.trim_start_matches("90").trim_start_matches('0'));
    let rows = statement.query_map(&[(":query", &query as &dyn rusqlite::ToSql), (":like", &like), (":phone", &phone), (":limit", &limit)], customer_from_row)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

#[tauri::command]
fn staff_create(input: StaffInput, state: tauri::State<'_, AppState>) -> Result<Staff, AppError> {
    let connection = state.sqlite.lock().expect("sqlite mutex poisoned");
    create_staff_tx(&connection, input)
}

#[tauri::command]
fn staff_update(id: String, input: StaffInput, state: tauri::State<'_, AppState>) -> Result<Staff, AppError> {
    let connection = state.sqlite.lock().expect("sqlite mutex poisoned");
    update_staff_tx(&connection, &id, input)
}

#[tauri::command]
fn staff_set_active(id: String, is_active: bool, state: tauri::State<'_, AppState>) -> Result<Staff, AppError> {
    let connection = state.sqlite.lock().expect("sqlite mutex poisoned");
    connection.execute("UPDATE staff SET is_active=?1, updated_at=?2 WHERE id=?3", params![bool_to_i64(is_active), now_iso(), id])?;
    get_staff(&connection, &id)?.ok_or_else(|| AppError::NotFound("STAFF_NOT_FOUND".to_string()))
}

#[tauri::command]
fn staff_list(status: Option<String>, limit: Option<i64>, state: tauri::State<'_, AppState>) -> Result<Vec<Staff>, AppError> {
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
fn staff_set_services(staff_id: String, service_ids: Vec<String>, state: tauri::State<'_, AppState>) -> Result<Vec<String>, AppError> {
    let mut connection = state.sqlite.lock().expect("sqlite mutex poisoned");
    set_staff_services_tx(&mut connection, &staff_id, service_ids)
}

#[tauri::command]
fn service_category_create(input: CategoryInput, state: tauri::State<'_, AppState>) -> Result<ServiceCategory, AppError> {
    let connection = state.sqlite.lock().expect("sqlite mutex poisoned");
    create_category_tx(&connection, input)
}

#[tauri::command]
fn service_create(input: ServiceInput, state: tauri::State<'_, AppState>) -> Result<ServiceItem, AppError> {
    let connection = state.sqlite.lock().expect("sqlite mutex poisoned");
    create_service_tx(&connection, input)
}

#[tauri::command]
fn service_update(id: String, input: ServiceInput, state: tauri::State<'_, AppState>) -> Result<ServiceItem, AppError> {
    let connection = state.sqlite.lock().expect("sqlite mutex poisoned");
    update_service_tx(&connection, &id, input)
}

#[tauri::command]
fn service_set_active(id: String, is_active: bool, state: tauri::State<'_, AppState>) -> Result<ServiceItem, AppError> {
    let connection = state.sqlite.lock().expect("sqlite mutex poisoned");
    connection.execute("UPDATE services SET is_active=?1, updated_at=?2 WHERE id=?3", params![bool_to_i64(is_active), now_iso(), id])?;
    get_service(&connection, &id)?.ok_or_else(|| AppError::NotFound("SERVICE_NOT_FOUND".to_string()))
}

#[tauri::command]
fn service_list(state: tauri::State<'_, AppState>) -> Result<Vec<ServiceItem>, AppError> {
    let connection = state.sqlite.lock().expect("sqlite mutex poisoned");
    list_service_items(&connection)
}

#[tauri::command]
fn appointment_create(input: AppointmentInput, state: tauri::State<'_, AppState>) -> Result<AppointmentSummary, AppError> {
    let mut connection = state.sqlite.lock().expect("sqlite mutex poisoned");
    create_appointment_tx(&mut connection, input)
}

#[tauri::command]
fn appointment_update(id: String, input: AppointmentInput, state: tauri::State<'_, AppState>) -> Result<AppointmentSummary, AppError> {
    let mut connection = state.sqlite.lock().expect("sqlite mutex poisoned");
    update_appointment_tx(&mut connection, &id, input)
}

#[tauri::command]
fn appointment_list_by_date(local_date: String, staff_id: Option<String>, limit: Option<i64>, state: tauri::State<'_, AppState>) -> Result<Vec<AppointmentSummary>, AppError> {
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
            appointment_list_by_date
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
        let category = create_category_tx(connection, CategoryInput { name: "Cilt Bakimi".into(), is_active: Some(true) }).expect("category");
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
        assert_eq!(read_schema_version(&connection).expect("schema"), CORE_SCHEMA_VERSION);
        for table in [
            "app_meta",
            "customers",
            "service_categories",
            "services",
            "staff",
            "staff_services",
            "appointments",
            "appointment_services",
        ] {
            let count: i64 = connection
                .query_row("SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1", params![table], |row| row.get(0))
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
        assert_eq!(read_schema_version(&connection).expect("schema"), CORE_SCHEMA_VERSION);
        let initialized_at: String = connection
            .query_row("SELECT initialized_at FROM app_meta WHERE id = 1", [], |row| row.get(0))
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

        let inactive = customer_set_active_for_test(&connection, &customer.id, false).expect("inactive");
        assert!(!inactive.is_active);
    }

    fn customer_set_active_for_test(connection: &Connection, id: &str, is_active: bool) -> Result<Customer, AppError> {
        connection.execute("UPDATE customers SET is_active=?1, updated_at=?2 WHERE id=?3", params![bool_to_i64(is_active), now_iso(), id])?;
        get_customer(connection, id)?.ok_or_else(|| AppError::NotFound("CUSTOMER_NOT_FOUND".to_string()))
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
        assert_eq!(appointment.services[0].service_name_snapshot, "Klasik Bakim");

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
        assert_eq!(get_appointment(&reopened, &appointment.id).expect("read").status, "cancelled");
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

        assert_eq!(customer_search_for_test(&connection, "555", "all").expect("customers").len(), 1);
        assert_eq!(staff_list_for_test(&connection).expect("staff").len(), 1);
        assert_eq!(list_service_items(&connection).expect("services").len(), 1);
        assert_eq!(
            list_appointments_between(&connection, Some("2026-08-24T00:00:00.000Z"), Some("2026-08-25T00:00:00.000Z"), None, None, None, 10)
                .expect("appointments")
                .len(),
            1
        );
        assert!(!appointment.services.is_empty());
    }

    fn customer_search_for_test(connection: &Connection, search: &str, status: &str) -> Result<Vec<Customer>, AppError> {
        let status_sql = if status == "all" { "1 = 1" } else { "is_active = 1" };
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
        let before: i64 = connection.query_row("SELECT COUNT(*) FROM appointments", [], |row| row.get(0)).expect("count");
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
        let after: i64 = connection.query_row("SELECT COUNT(*) FROM appointments", [], |row| row.get(0)).expect("count");
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
        let (database_path, backups_dir, logs_dir, exports_dir, settings_dir) = data_paths(temp.path()).expect("paths");
        assert_eq!(database_path.file_name().and_then(|name| name.to_str()), Some("salon-foundation.db"));
        assert!(backups_dir.is_dir());
        assert!(logs_dir.is_dir());
        assert!(exports_dir.is_dir());
        assert!(settings_dir.is_dir());
    }
}
