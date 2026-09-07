use crate::{utc_iso, AppError, AppointmentSummary};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::VecDeque;
use url::Url;

pub const GOOGLE_AUTH_ENDPOINT: &str = "https://accounts.google.com/o/oauth2/v2/auth";
pub const GOOGLE_TOKEN_ENDPOINT: &str = "https://oauth2.googleapis.com/token";
pub const GOOGLE_CALENDAR_API_BASE: &str = "https://www.googleapis.com/calendar/v3";
pub const GOOGLE_CALENDAR_SCOPE: &str = "https://www.googleapis.com/auth/calendar.events";

#[derive(Debug, Clone)]
pub struct HttpRequest {
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct HttpResponse {
    pub status: u16,
    pub body: Vec<u8>,
}

pub trait HttpTransport {
    fn send(&mut self, request: HttpRequest) -> Result<HttpResponse, AppError>;
}

#[derive(Debug, Default)]
pub struct ReqwestHttpTransport;

impl HttpTransport for ReqwestHttpTransport {
    fn send(&mut self, request: HttpRequest) -> Result<HttpResponse, AppError> {
        tauri::async_runtime::block_on(async move {
            let method = reqwest::Method::from_bytes(request.method.as_bytes())
                .map_err(|error| AppError::Validation(format!("HTTP_METHOD_INVALID: {error}")))?;
            let client = reqwest::Client::new();
            let mut builder = client.request(method, &request.url);
            for (key, value) in request.headers {
                builder = builder.header(key, value);
            }
            let response = builder
                .body(request.body)
                .send()
                .await
                .map_err(|_| AppError::Database("NETWORK_ERROR".to_string()))?;
            let status = response.status().as_u16();
            let body = response
                .bytes()
                .await
                .map_err(|_| AppError::Database("NETWORK_ERROR".to_string()))?
                .to_vec();
            Ok(HttpResponse { status, body })
        })
    }
}

#[derive(Debug, Default)]
pub struct FakeHttpTransport {
    pub requests: Vec<HttpRequest>,
    responses: VecDeque<HttpResponse>,
}

impl FakeHttpTransport {
    pub fn new(responses: Vec<HttpResponse>) -> Self {
        Self {
            requests: Vec::new(),
            responses: responses.into(),
        }
    }
}

impl HttpTransport for FakeHttpTransport {
    fn send(&mut self, request: HttpRequest) -> Result<HttpResponse, AppError> {
        self.requests.push(request);
        self.responses
            .pop_front()
            .ok_or_else(|| AppError::Database("FAKE_HTTP_RESPONSE_MISSING".to_string()))
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GoogleConnectionStatus {
    pub configured: bool,
    pub connected: bool,
    pub pending_sync_count: i64,
    pub blocked_sync_count: i64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GoogleOAuthConfig {
    pub client_id: String,
    #[serde(default)]
    pub client_secret: String,
    pub calendar_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GoogleTokenSet {
    pub refresh_token: String,
    pub access_token: Option<String>,
}

pub fn token_exchange_failure_code(response: &HttpResponse) -> String {
    let response_data = serde_json::from_slice::<Value>(&response.body).ok();
    let oauth_error = response_data
        .as_ref()
        .and_then(|data| data.get("error").and_then(Value::as_str).map(str::to_owned))
        .filter(|value| {
            !value.is_empty()
                && value.len() <= 64
                && value.chars().all(|character| {
                    character.is_ascii_alphanumeric() || character == '_' || character == '-'
                })
        })
        .unwrap_or_else(|| "unknown".to_string());
    let description_category = response_data
        .as_ref()
        .and_then(|data| data.get("error_description").and_then(Value::as_str))
        .map(classify_token_error_description)
        .unwrap_or("OTHER");
    format!(
        "GOOGLE_TOKEN_EXCHANGE_FAILED_HTTP:{}:{oauth_error}:{description_category}",
        response.status
    )
}

pub fn classify_token_error_description(description: &str) -> &'static str {
    let text = description.to_ascii_lowercase();
    let has_secret = text.contains("client_secret") || text.contains("client secret");
    let missing = text.contains("missing") || text.contains("required") || text.contains("absent");
    let invalid = text.contains("invalid") || text.contains("incorrect");
    if has_secret && missing {
        "CLIENT_SECRET_MISSING"
    } else if has_secret && invalid {
        "CLIENT_SECRET_INVALID"
    } else if text.contains("code_verifier")
        || text.contains("code verifier")
        || text.contains("pkce")
    {
        "CODE_VERIFIER_PROBLEM"
    } else if text.contains("redirect_uri") || text.contains("redirect uri") {
        "REDIRECT_URI_PROBLEM"
    } else if text.contains("client_id") || text.contains("client id") {
        "CLIENT_ID_PROBLEM"
    } else if text.contains("grant_type") || text.contains("grant type") {
        "GRANT_TYPE_PROBLEM"
    } else if text.contains("unsupported")
        && (text.contains("authorization")
            || text.contains("authentication")
            || text.contains("auth method"))
    {
        "UNSUPPORTED_AUTH_METHOD"
    } else if missing {
        "MISSING_PARAMETER_OTHER"
    } else {
        "OTHER"
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PkceCodes {
    pub verifier: String,
    pub challenge: String,
}

pub fn is_valid_pkce_verifier(verifier: &str) -> bool {
    (43..=128).contains(&verifier.len())
        && verifier
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.' || c == '_' || c == '~')
}

pub fn derive_pkce_challenge(verifier: &str) -> String {
    let hash = Sha256::digest(verifier.as_bytes());
    URL_SAFE_NO_PAD.encode(hash)
}

fn secure_random_bytes(buffer: &mut [u8]) -> Result<(), AppError> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Security::Cryptography::{
            BCryptGenRandom, BCRYPT_USE_SYSTEM_PREFERRED_RNG,
        };
        let status = unsafe {
            BCryptGenRandom(
                std::ptr::null_mut(),
                buffer.as_mut_ptr(),
                buffer.len() as u32,
                BCRYPT_USE_SYSTEM_PREFERRED_RNG,
            )
        };
        if status != 0 {
            return Err(AppError::Io(std::io::Error::last_os_error()));
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        use std::io::Read;
        let mut file = std::fs::File::open("/dev/urandom")?;
        file.read_exact(buffer)?;
        Ok(())
    }
}

pub fn generate_pkce() -> Result<PkceCodes, AppError> {
    let mut random_bytes = [0u8; 32];
    secure_random_bytes(&mut random_bytes)?;
    let verifier = URL_SAFE_NO_PAD.encode(random_bytes);
    let challenge = derive_pkce_challenge(&verifier);
    Ok(PkceCodes {
        verifier,
        challenge,
    })
}

pub fn build_google_auth_url(
    client_id: &str,
    redirect_uri: &str,
    state: &str,
    code_challenge: &str,
) -> Result<String, AppError> {
    let mut url = Url::parse(GOOGLE_AUTH_ENDPOINT)
        .map_err(|_| AppError::Validation("GOOGLE_AUTH_URL_INVALID".to_string()))?;
    url.query_pairs_mut()
        .append_pair("client_id", client_id)
        .append_pair("redirect_uri", redirect_uri)
        .append_pair("response_type", "code")
        .append_pair("scope", GOOGLE_CALENDAR_SCOPE)
        .append_pair("access_type", "offline")
        .append_pair("prompt", "consent")
        .append_pair("state", state)
        .append_pair("code_challenge", code_challenge)
        .append_pair("code_challenge_method", "S256");
    Ok(url.to_string())
}

pub fn parse_oauth_callback(callback_url: &str, expected_state: &str) -> Result<String, AppError> {
    let url = Url::parse(callback_url)
        .map_err(|_| AppError::Validation("GOOGLE_CALLBACK_INVALID".to_string()))?;
    if url.path() != "/oauth2callback" {
        return Err(AppError::Validation(
            "GOOGLE_CALLBACK_PATH_INVALID".to_string(),
        ));
    }
    let state = url
        .query_pairs()
        .find(|(key, _)| key == "state")
        .map(|(_, value)| value.to_string());
    let code = url
        .query_pairs()
        .find(|(key, _)| key == "code")
        .map(|(_, value)| value.to_string());
    if state.as_deref() != Some(expected_state) {
        return Err(AppError::Validation(
            "GOOGLE_OAUTH_STATE_INVALID".to_string(),
        ));
    }
    code.filter(|value| !value.is_empty())
        .ok_or_else(|| AppError::Validation("GOOGLE_OAUTH_CODE_MISSING".to_string()))
}

pub fn exchange_auth_code(
    transport: &mut dyn HttpTransport,
    config: &GoogleOAuthConfig,
    redirect_uri: &str,
    code: &str,
    code_verifier: &str,
) -> Result<GoogleTokenSet, AppError> {
    if config.client_id.trim().is_empty()
        || redirect_uri.trim().is_empty()
        || code.trim().is_empty()
        || !is_valid_pkce_verifier(code_verifier)
    {
        return Err(AppError::Validation(
            "GOOGLE_TOKEN_REQUEST_INVALID".to_string(),
        ));
    }
    let mut form = url::form_urlencoded::Serializer::new(String::new());
    form.append_pair("code", code)
        .append_pair("client_id", &config.client_id)
        .append_pair("redirect_uri", redirect_uri)
        .append_pair("grant_type", "authorization_code")
        .append_pair("code_verifier", code_verifier);
    if !config.client_secret.trim().is_empty() {
        form.append_pair("client_secret", &config.client_secret);
    }
    let body = form.finish();
    let response = transport.send(HttpRequest {
        method: "POST".to_string(),
        url: GOOGLE_TOKEN_ENDPOINT.to_string(),
        headers: vec![(
            "Content-Type".to_string(),
            "application/x-www-form-urlencoded".to_string(),
        )],
        body: body.into_bytes(),
    })?;
    if response.status != 200 {
        return Err(AppError::Database(token_exchange_failure_code(&response)));
    }
    let data: Value = serde_json::from_slice(&response.body)
        .map_err(|_| AppError::Database("GOOGLE_TOKEN_RESPONSE_INVALID".to_string()))?;
    let refresh_token = data
        .get("refresh_token")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| AppError::Database("GOOGLE_REFRESH_TOKEN_MISSING".to_string()))?;
    Ok(GoogleTokenSet {
        refresh_token: refresh_token.to_string(),
        access_token: data
            .get("access_token")
            .and_then(Value::as_str)
            .map(str::to_string),
    })
}

pub fn refresh_access_token(
    transport: &mut dyn HttpTransport,
    config: &GoogleOAuthConfig,
    refresh_token: &str,
) -> Result<String, AppError> {
    let mut form = url::form_urlencoded::Serializer::new(String::new());
    form.append_pair("client_id", &config.client_id)
        .append_pair("refresh_token", refresh_token)
        .append_pair("grant_type", "refresh_token");
    if !config.client_secret.trim().is_empty() {
        form.append_pair("client_secret", &config.client_secret);
    }
    let body = form.finish();
    let response = transport.send(HttpRequest {
        method: "POST".to_string(),
        url: GOOGLE_TOKEN_ENDPOINT.to_string(),
        headers: vec![(
            "Content-Type".to_string(),
            "application/x-www-form-urlencoded".to_string(),
        )],
        body: body.into_bytes(),
    })?;
    if response.status != 200 {
        return Err(AppError::Database(
            "GOOGLE_CALENDAR_AUTH_REQUIRED".to_string(),
        ));
    }
    let data: Value = serde_json::from_slice(&response.body)
        .map_err(|_| AppError::Database("GOOGLE_TOKEN_RESPONSE_INVALID".to_string()))?;
    data.get("access_token")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| AppError::Database("GOOGLE_ACCESS_TOKEN_MISSING".to_string()))
}

pub fn build_calendar_event(appointment: &AppointmentSummary) -> Value {
    let cancelled = appointment.status == "cancelled";
    json!({
        "summary": format!("{}BeautySaloon TEST - {}", if cancelled { "IPTAL - " } else { "" }, appointment.customer_name),
        "description": format!("BeautySaloon appointment_id={}", appointment.id),
        "start": { "dateTime": appointment.start_at_utc, "timeZone": "Europe/Istanbul" },
        "end": { "dateTime": appointment.end_at_utc, "timeZone": "Europe/Istanbul" },
        "extendedProperties": {
            "private": {
                "beautysaloonAppointmentId": appointment.id,
                "beautysaloonManaged": "true"
            }
        }
    })
}

pub fn upsert_calendar_event(
    transport: &mut dyn HttpTransport,
    access_token: &str,
    calendar_id: &str,
    google_event_id: Option<&str>,
    event: &Value,
) -> Result<String, AppError> {
    let calendar = urlencoding(calendar_id);
    let (method, url) = if let Some(event_id) = google_event_id {
        (
            "PATCH",
            format!(
                "{GOOGLE_CALENDAR_API_BASE}/calendars/{calendar}/events/{}",
                urlencoding(event_id)
            ),
        )
    } else {
        (
            "POST",
            format!("{GOOGLE_CALENDAR_API_BASE}/calendars/{calendar}/events"),
        )
    };
    let response = transport.send(HttpRequest {
        method: method.to_string(),
        url,
        headers: vec![
            (
                "Authorization".to_string(),
                format!("Bearer {access_token}"),
            ),
            ("Content-Type".to_string(), "application/json".to_string()),
        ],
        body: serde_json::to_vec(event)
            .map_err(|_| AppError::Database("GOOGLE_EVENT_SERIALIZE_FAILED".to_string()))?,
    })?;
    if response.status == 401 {
        return Err(AppError::Database(
            "GOOGLE_CALENDAR_AUTH_REQUIRED".to_string(),
        ));
    }
    if !(200..300).contains(&response.status) {
        return Err(calendar_api_error(&response));
    }
    let data: Value = serde_json::from_slice(&response.body)
        .map_err(|_| AppError::Database("GOOGLE_EVENT_RESPONSE_INVALID".to_string()))?;
    data.get("id")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| AppError::Database("GOOGLE_EVENT_ID_MISSING".to_string()))
}

fn calendar_api_error(response: &HttpResponse) -> AppError {
    let data: Value = serde_json::from_slice(&response.body).unwrap_or(Value::Null);
    let error = data.get("error").unwrap_or(&Value::Null);
    let code = error
        .get("code")
        .and_then(Value::as_i64)
        .unwrap_or(response.status as i64);
    let reason = error
        .get("errors")
        .and_then(Value::as_array)
        .and_then(|errors| errors.first())
        .and_then(|item| item.get("reason"))
        .and_then(Value::as_str)
        .map(sanitize_google_error_part)
        .unwrap_or_else(|| "unknown".to_string());
    let message = error
        .get("message")
        .and_then(Value::as_str)
        .map(sanitize_google_error_part)
        .unwrap_or_else(|| "no_message".to_string());
    AppError::Database(format!(
        "GOOGLE_CALENDAR_API_HTTP_{code}_{reason}_{message}"
    ))
}

fn sanitize_google_error_part(value: &str) -> String {
    let sanitized: String = value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character
            } else {
                '_'
            }
        })
        .collect();
    sanitized.chars().take(96).collect()
}

pub fn reject_unrelated_event_mutation(
    connection: &Connection,
    appointment_id: &str,
    google_event_id: &str,
) -> Result<(), AppError> {
    let mapped: Option<String> = connection
        .query_row(
            "SELECT google_event_id FROM appointment_google_calendar_sync WHERE appointment_id=?1",
            params![appointment_id],
            |row| row.get(0),
        )
        .optional()?
        .flatten();
    match mapped {
        Some(mapped) if mapped == google_event_id => Ok(()),
        _ => Err(AppError::Validation(
            "GOOGLE_UNRELATED_EVENT_REJECTED".to_string(),
        )),
    }
}

pub fn google_connection_status(
    connection: &Connection,
    secure_storage_available: bool,
) -> Result<GoogleConnectionStatus, AppError> {
    let (client_id, sync_enabled): (Option<String>, i64) = connection.query_row(
        "SELECT client_id, sync_enabled FROM google_calendar_settings WHERE id=1",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let refresh_count: i64 = connection.query_row(
        "SELECT COUNT(*) FROM secure_secrets WHERE secret_key='google_calendar_refresh_token'",
        [],
        |row| row.get(0),
    )?;
    let pending: i64 = connection.query_row(
        "SELECT COUNT(*) FROM google_calendar_outbox WHERE sync_status IN ('pending','in_flight')",
        [],
        |row| row.get(0),
    )?;
    let blocked: i64 = connection.query_row(
        "SELECT COUNT(*) FROM google_calendar_outbox WHERE sync_status='blocked'",
        [],
        |row| row.get(0),
    )?;
    Ok(GoogleConnectionStatus {
        configured: client_id.is_some(),
        connected: sync_enabled == 1 && refresh_count > 0 && secure_storage_available,
        pending_sync_count: pending,
        blocked_sync_count: blocked,
    })
}

fn urlencoding(value: &str) -> String {
    url::form_urlencoded::byte_serialize(value.as_bytes()).collect()
}

pub fn mark_google_outbox_synced(
    connection: &Connection,
    appointment_id: &str,
    event_id: &str,
    appointment_updated_at: &str,
) -> Result<(), AppError> {
    let now = utc_iso(chrono::Utc::now());
    connection.execute(
        "UPDATE google_calendar_outbox SET sync_status='synced', last_error_code=NULL, updated_at_utc=?1 WHERE appointment_id=?2",
        params![now, appointment_id],
    )?;
    connection.execute(
        "UPDATE appointment_google_calendar_sync SET google_event_id=?1, last_synced_updated_at_utc=?2, last_error_code=NULL, updated_at_utc=?3 WHERE appointment_id=?4",
        params![event_id, appointment_updated_at, now, appointment_id],
    )?;
    Ok(())
}
