use chrono::Utc;
use rusqlite::{params, Connection};
use serde::Serialize;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tauri::{AppHandle, Manager};
use thiserror::Error;

const SCHEMA_VERSION: i64 = 1;

#[derive(Debug, Error)]
enum FoundationError {
    #[error("App data yolu hazirlanamadi: {0}")]
    AppDataPath(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
}

impl Serialize for FoundationError {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
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
pub struct FoundationNote {
    id: i64,
    title: String,
    created_at_utc: String,
}

#[derive(Debug)]
pub struct AppState {
    database_path: PathBuf,
    sqlite: Mutex<Connection>,
}

fn app_data_root(app: &AppHandle) -> Result<PathBuf, FoundationError> {
    app.path()
        .app_data_dir()
        .map_err(|error| FoundationError::AppDataPath(error.to_string()))
}

fn data_paths(root: &Path) -> Result<(PathBuf, PathBuf, PathBuf, PathBuf, PathBuf), FoundationError> {
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

fn open_database(path: &Path) -> Result<Connection, FoundationError> {
    let connection = Connection::open(path)?;
    connection.pragma_update(None, "journal_mode", "WAL")?;
    connection.pragma_update(None, "foreign_keys", "ON")?;
    migrate_foundation(&connection)?;
    Ok(connection)
}

fn migrate_foundation(connection: &Connection) -> Result<(), FoundationError> {
    connection.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS app_meta (
          id INTEGER PRIMARY KEY CHECK (id = 1),
          schema_version INTEGER NOT NULL,
          created_at_utc TEXT NOT NULL,
          updated_at_utc TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS foundation_notes (
          id INTEGER PRIMARY KEY AUTOINCREMENT,
          title TEXT NOT NULL,
          created_at_utc TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS audit_log (
          id INTEGER PRIMARY KEY AUTOINCREMENT,
          entity_type TEXT NOT NULL,
          entity_id TEXT NOT NULL,
          action TEXT NOT NULL,
          occurred_at_utc TEXT NOT NULL,
          summary TEXT NOT NULL
        );

        CREATE INDEX IF NOT EXISTS foundation_notes_created_idx
          ON foundation_notes (created_at_utc DESC, id DESC);
        CREATE INDEX IF NOT EXISTS audit_log_entity_idx
          ON audit_log (entity_type, entity_id, occurred_at_utc DESC);
        ",
    )?;

    let now = Utc::now().to_rfc3339();
    connection.execute(
        "
        INSERT INTO app_meta (id, schema_version, created_at_utc, updated_at_utc)
        VALUES (1, ?1, ?2, ?2)
        ON CONFLICT(id) DO UPDATE SET
          schema_version = excluded.schema_version,
          updated_at_utc = excluded.updated_at_utc
        ",
        params![SCHEMA_VERSION, now],
    )?;

    Ok(())
}

fn integrity_check(connection: &Connection) -> Result<bool, FoundationError> {
    let result: String = connection.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
    Ok(result == "ok")
}

fn read_schema_version(connection: &Connection) -> Result<i64, FoundationError> {
    connection.query_row("SELECT schema_version FROM app_meta WHERE id = 1", [], |row| row.get(0)).map_err(Into::into)
}

#[tauri::command]
fn app_health(app: AppHandle, state: tauri::State<'_, AppState>) -> Result<AppHealth, FoundationError> {
    let connection = state.sqlite.lock().expect("sqlite mutex poisoned");
    let schema_version = read_schema_version(&connection)?;
    let ok = integrity_check(&connection)?;

    Ok(AppHealth {
        ok,
        app_version: app.package_info().version.to_string(),
        schema_version,
        database_path: state.database_path.display().to_string(),
        message: if ok {
            "SQLite foundation hazir.".to_string()
        } else {
            "SQLite integrity check basarisiz.".to_string()
        },
    })
}

#[tauri::command]
fn create_foundation_note(title: String, state: tauri::State<'_, AppState>) -> Result<FoundationNote, FoundationError> {
    let normalized = title.trim();
    let title = if normalized.is_empty() {
        "Foundation smoke".to_string()
    } else {
        normalized.chars().take(180).collect::<String>()
    };
    let now = Utc::now().to_rfc3339();
    let connection = state.sqlite.lock().expect("sqlite mutex poisoned");

    connection.execute(
        "INSERT INTO foundation_notes (title, created_at_utc) VALUES (?1, ?2)",
        params![title, now],
    )?;
    let id = connection.last_insert_rowid();
    connection.execute(
        "
        INSERT INTO audit_log (entity_type, entity_id, action, occurred_at_utc, summary)
        VALUES ('foundation_note', ?1, 'create', ?2, 'SQLite foundation smoke write')
        ",
        params![id.to_string(), now],
    )?;

    Ok(FoundationNote {
        id,
        title,
        created_at_utc: now,
    })
}

#[tauri::command]
fn list_foundation_notes(limit: Option<u32>, state: tauri::State<'_, AppState>) -> Result<Vec<FoundationNote>, FoundationError> {
    let bounded_limit = limit.unwrap_or(10).clamp(1, 50);
    let connection = state.sqlite.lock().expect("sqlite mutex poisoned");
    let mut statement = connection.prepare(
        "
        SELECT id, title, created_at_utc
        FROM foundation_notes
        ORDER BY created_at_utc DESC, id DESC
        LIMIT ?1
        ",
    )?;

    let rows = statement.query_map(params![bounded_limit], |row| {
        Ok(FoundationNote {
            id: row.get(0)?,
            title: row.get(1)?,
            created_at_utc: row.get(2)?,
        })
    })?;

    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
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
            create_foundation_note,
            list_foundation_notes
        ])
        .run(tauri::generate_context!())
        .expect("error while running BeautySaloon");
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn opens_migrates_and_writes_foundation_sqlite() {
        let temp = tempdir().expect("tempdir");
        let database_path = temp.path().join("foundation.db");
        let connection = open_database(&database_path).expect("open database");

        assert_eq!(read_schema_version(&connection).expect("schema version"), SCHEMA_VERSION);
        assert!(integrity_check(&connection).expect("integrity"));

        connection
            .execute(
                "INSERT INTO foundation_notes (title, created_at_utc) VALUES (?1, ?2)",
                params!["smoke", Utc::now().to_rfc3339()],
            )
            .expect("insert note");

        let count: i64 = connection
            .query_row("SELECT COUNT(*) FROM foundation_notes", [], |row| row.get(0))
            .expect("count");
        assert_eq!(count, 1);
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
