use crate::services::google::{HttpRequest, HttpTransport};
use crate::services::supabase::{auth_headers, SupabaseConfig, SupabaseSession};
use crate::AppError;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CloudSyncStatus {
    pub processed: u32,
    pub blocked: u32,
    pub unauthorized: bool,
}

pub fn invoke_function(
    transport: &mut dyn HttpTransport,
    config: &SupabaseConfig,
    session: &SupabaseSession,
    function_name: &str,
    body: Value,
) -> Result<Value, AppError> {
    let url = format!("{}/functions/v1/{function_name}", config.project_url.trim_end_matches('/'));
    let response = transport.send(HttpRequest {
        method: "POST".to_string(),
        url,
        headers: auth_headers(config, Some(&session.access_token)),
        body: serde_json::to_vec(&body).map_err(|_| AppError::Database("CLOUD_REQUEST_SERIALIZE_FAILED".to_string()))?,
    })?;
    match response.status {
        200..=299 => serde_json::from_slice(&response.body).map_err(|_| AppError::Database("CLOUD_RESPONSE_INVALID".to_string())),
        401 | 403 => Err(AppError::Validation("CLOUD_AUTH_INVALID".to_string())),
        429 => Err(AppError::Database("CLOUD_RATE_LIMITED".to_string())),
        500..=599 => Err(AppError::Database("CLOUD_TEMPORARILY_UNAVAILABLE".to_string())),
        _ => Err(AppError::Database("CLOUD_OPERATION_FAILED".to_string())),
    }
}

pub fn process_ordered_outbox(
    connection: &Connection,
    transport: &mut dyn HttpTransport,
    config: &SupabaseConfig,
    session: &SupabaseSession,
    limit: i64,
) -> Result<CloudSyncStatus, AppError> {
    let mut statement = connection.prepare(
        "SELECT current.id, current.reminder_id, current.revision, current.action, current.client_mutation_id, current.payload_json
         FROM reminder_cloud_outbox current
         WHERE current.sync_status='pending'
           AND NOT EXISTS (
             SELECT 1 FROM reminder_cloud_outbox previous
             WHERE previous.reminder_id=current.reminder_id
               AND previous.revision < current.revision
               AND previous.sync_status IN ('pending','in_flight','blocked')
           )
         ORDER BY current.created_at_utc ASC, current.reminder_id ASC, current.revision ASC
         LIMIT ?1",
    )?;
    let rows = statement
        .query_map(params![limit], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, Option<String>>(5)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let mut status = CloudSyncStatus {
        processed: 0,
        blocked: 0,
        unauthorized: false,
    };
    for (id, reminder_id, revision, action, mutation_id, payload_json) in rows {
        connection.execute(
            "UPDATE reminder_cloud_outbox SET sync_status='in_flight', last_attempt_at_utc=strftime('%Y-%m-%dT%H:%M:%fZ','now'), last_error_code=NULL WHERE id=?1 AND sync_status='pending'",
            params![id],
        )?;
        let body = if action == "cancel" {
            json!({ "apiVersion": 1, "reminderId": reminder_id, "revision": revision, "clientMutationId": mutation_id })
        } else {
            serde_json::from_str(payload_json.as_deref().ok_or_else(|| AppError::Database("CLOUD_PAYLOAD_MISSING".to_string()))?)
                .map_err(|_| AppError::Database("CLOUD_PAYLOAD_INVALID".to_string()))?
        };
        let function_name = if action == "cancel" { "reminder-cancel" } else { "reminder-upsert" };
        match invoke_function(transport, config, session, function_name, body) {
            Ok(response) => {
                let data = response.get("data").unwrap_or(&response);
                let remote_revision = data.get("revision").and_then(Value::as_i64).unwrap_or(revision);
                let remote_status = data.get("status").and_then(Value::as_str).unwrap_or(if action == "cancel" { "cancelled" } else { "pending" });
                let remote_updated = data.get("remoteUpdatedAtUtc").and_then(Value::as_str).unwrap_or("");
                connection.execute("UPDATE reminder_cloud_outbox SET sync_status='synced', last_error_code=NULL, updated_at_utc=strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id=?1", params![id])?;
                connection.execute(
                    "UPDATE reminder_cloud_state
                     SET last_synced_revision=max(last_synced_revision, ?1),
                         last_remote_revision=?2,
                         last_remote_status=?3,
                         last_remote_updated_at_utc=CASE WHEN ?4='' THEN last_remote_updated_at_utc ELSE ?4 END,
                         last_error_code=NULL,
                         updated_at_utc=strftime('%Y-%m-%dT%H:%M:%fZ','now')
                     WHERE reminder_id=?5",
                    params![revision, remote_revision, remote_status, remote_updated, reminder_id],
                )?;
                status.processed += 1;
            }
            Err(AppError::Validation(code)) if code == "CLOUD_AUTH_INVALID" => {
                connection.execute("UPDATE reminder_cloud_outbox SET sync_status='blocked', last_error_code=?1 WHERE id=?2", params![code, id])?;
                status.unauthorized = true;
                status.blocked += 1;
                break;
            }
            Err(error) => {
                let code = error.to_string();
                connection.execute("UPDATE reminder_cloud_outbox SET sync_status='pending', last_error_code=?1 WHERE id=?2", params![code, id])?;
                break;
            }
        }
    }
    Ok(status)
}

pub fn reconcile_remote_status(connection: &Connection, reminder_id: &str, revision: i64, remote_status: &str, remote_updated_at: &str) -> Result<(), AppError> {
    connection.execute(
        "UPDATE reminder_cloud_state
         SET last_remote_revision=?1, last_remote_status=?2, last_remote_updated_at_utc=?3, last_error_code=NULL, updated_at_utc=strftime('%Y-%m-%dT%H:%M:%fZ','now')
         WHERE reminder_id=?4",
        params![revision, remote_status, remote_updated_at, reminder_id],
    )?;
    if matches!(remote_status, "sent" | "failed" | "uncertain") {
        connection.execute(
            "UPDATE appointment_reminders
             SET status=?1,
                 sent_at_utc=CASE WHEN ?1='sent' THEN ?2 ELSE sent_at_utc END,
                 failure_code=CASE WHEN ?1 IN ('failed','uncertain') THEN 'REMOTE_STATUS' ELSE failure_code END,
                 updated_at=?2
             WHERE id=?3 AND status IN ('pending','processing')",
            params![remote_status, remote_updated_at, reminder_id],
        )?;
    }
    Ok(())
}

pub fn has_session_secret(connection: &Connection) -> Result<bool, AppError> {
    let count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM secure_secrets WHERE secret_key='cloud_supabase_session'",
            [],
            |row| row.get(0),
        )
        .optional()?
        .unwrap_or(0);
    Ok(count > 0)
}

