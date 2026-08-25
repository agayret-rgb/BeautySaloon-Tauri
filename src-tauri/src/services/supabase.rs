use crate::services::google::{HttpRequest, HttpTransport};
use crate::AppError;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SupabaseConfig {
    pub project_url: String,
    pub publishable_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SupabaseSession {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SupabaseConnectionStatus {
    pub configured: bool,
    pub session_present: bool,
}

pub fn validate_public_config(config: &SupabaseConfig) -> Result<(), AppError> {
    if !config.project_url.starts_with("https://") || config.publishable_key.trim().len() < 20 {
        return Err(AppError::Validation("SUPABASE_CONFIG_INVALID".to_string()));
    }
    Ok(())
}

pub fn request_email_otp(transport: &mut dyn HttpTransport, config: &SupabaseConfig, email: &str) -> Result<(), AppError> {
    validate_public_config(config)?;
    let url = format!("{}/auth/v1/otp", config.project_url.trim_end_matches('/'));
    let body = json!({ "email": email, "create_user": false }).to_string();
    let response = transport.send(HttpRequest {
        method: "POST".to_string(),
        url,
        headers: auth_headers(config, None),
        body: body.into_bytes(),
    })?;
    if !(200..300).contains(&response.status) {
        return Err(classify_supabase_status(response.status));
    }
    Ok(())
}

pub fn verify_email_otp(transport: &mut dyn HttpTransport, config: &SupabaseConfig, email: &str, token: &str) -> Result<SupabaseSession, AppError> {
    validate_public_config(config)?;
    let url = format!("{}/auth/v1/verify", config.project_url.trim_end_matches('/'));
    let body = json!({ "email": email, "token": token, "type": "email" }).to_string();
    let response = transport.send(HttpRequest {
        method: "POST".to_string(),
        url,
        headers: auth_headers(config, None),
        body: body.into_bytes(),
    })?;
    if !(200..300).contains(&response.status) {
        return Err(classify_supabase_status(response.status));
    }
    parse_session(&response.body)
}

pub fn refresh_session(transport: &mut dyn HttpTransport, config: &SupabaseConfig, refresh_token: &str) -> Result<SupabaseSession, AppError> {
    validate_public_config(config)?;
    let url = format!("{}/auth/v1/token?grant_type=refresh_token", config.project_url.trim_end_matches('/'));
    let body = json!({ "refresh_token": refresh_token }).to_string();
    let response = transport.send(HttpRequest {
        method: "POST".to_string(),
        url,
        headers: auth_headers(config, None),
        body: body.into_bytes(),
    })?;
    if !(200..300).contains(&response.status) {
        return Err(classify_supabase_status(response.status));
    }
    parse_session(&response.body)
}

pub fn validate_authenticated_session(transport: &mut dyn HttpTransport, config: &SupabaseConfig, access_token: &str) -> Result<(), AppError> {
    let url = format!("{}/auth/v1/user", config.project_url.trim_end_matches('/'));
    let response = transport.send(HttpRequest {
        method: "GET".to_string(),
        url,
        headers: auth_headers(config, Some(access_token)),
        body: Vec::new(),
    })?;
    if response.status == 200 {
        Ok(())
    } else {
        Err(classify_supabase_status(response.status))
    }
}

pub fn auth_headers(config: &SupabaseConfig, bearer: Option<&str>) -> Vec<(String, String)> {
    let mut headers = vec![
        ("apikey".to_string(), config.publishable_key.clone()),
        ("Content-Type".to_string(), "application/json".to_string()),
        ("x-client-info".to_string(), "beautysaloon-tauri".to_string()),
    ];
    if let Some(token) = bearer {
        headers.push(("Authorization".to_string(), format!("Bearer {token}")));
    }
    headers
}

fn parse_session(body: &[u8]) -> Result<SupabaseSession, AppError> {
    let data: Value = serde_json::from_slice(body).map_err(|_| AppError::Database("SUPABASE_SESSION_RESPONSE_INVALID".to_string()))?;
    Ok(SupabaseSession {
        access_token: data
            .get("access_token")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| AppError::Database("SUPABASE_ACCESS_TOKEN_MISSING".to_string()))?
            .to_string(),
        refresh_token: data
            .get("refresh_token")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| AppError::Database("SUPABASE_REFRESH_TOKEN_MISSING".to_string()))?
            .to_string(),
        expires_at: data.get("expires_at").and_then(Value::as_i64),
    })
}

fn classify_supabase_status(status: u16) -> AppError {
    match status {
        401 | 403 => AppError::Validation("SUPABASE_AUTH_INVALID".to_string()),
        404 => AppError::Validation("SUPABASE_USER_NOT_FOUND".to_string()),
        429 => AppError::Database("SUPABASE_RATE_LIMITED".to_string()),
        500..=599 => AppError::Database("SUPABASE_TEMPORARILY_UNAVAILABLE".to_string()),
        _ => AppError::Database("SUPABASE_OPERATION_FAILED".to_string()),
    }
}

