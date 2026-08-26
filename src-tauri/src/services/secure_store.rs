use crate::services::supabase::SupabaseSession;
use crate::AppError;
use base64::Engine;
use rusqlite::{params, Connection, OptionalExtension};

pub const PROVIDER_DPAPI: &str = "windows_dpapi_v1";

pub trait SecretProtector {
    fn protect(&self, plaintext: &[u8]) -> Result<Vec<u8>, AppError>;
    fn unprotect(&self, ciphertext: &[u8]) -> Result<Vec<u8>, AppError>;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct WindowsDpapiProtector;

#[cfg(windows)]
impl SecretProtector for WindowsDpapiProtector {
    fn protect(&self, plaintext: &[u8]) -> Result<Vec<u8>, AppError> {
        dpapi_protect(plaintext)
    }

    fn unprotect(&self, ciphertext: &[u8]) -> Result<Vec<u8>, AppError> {
        dpapi_unprotect(ciphertext)
    }
}

#[cfg(not(windows))]
impl SecretProtector for WindowsDpapiProtector {
    fn protect(&self, _plaintext: &[u8]) -> Result<Vec<u8>, AppError> {
        Err(AppError::Database("DPAPI_UNAVAILABLE".to_string()))
    }

    fn unprotect(&self, _ciphertext: &[u8]) -> Result<Vec<u8>, AppError> {
        Err(AppError::Database("DPAPI_UNAVAILABLE".to_string()))
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct TestProtector;

impl SecretProtector for TestProtector {
    fn protect(&self, plaintext: &[u8]) -> Result<Vec<u8>, AppError> {
        Ok(base64::engine::general_purpose::STANDARD
            .encode(plaintext)
            .into_bytes())
    }

    fn unprotect(&self, ciphertext: &[u8]) -> Result<Vec<u8>, AppError> {
        base64::engine::general_purpose::STANDARD
            .decode(ciphertext)
            .map_err(|_| AppError::Database("TEST_SECRET_DECRYPT_FAILED".to_string()))
    }
}

pub fn secure_storage_available(protector: &dyn SecretProtector) -> bool {
    protector
        .protect(b"beautysaloon-probe")
        .and_then(|cipher| protector.unprotect(&cipher))
        .map(|plain| plain == b"beautysaloon-probe")
        .unwrap_or(false)
}

pub fn upsert_secret(
    connection: &Connection,
    protector: &dyn SecretProtector,
    key: &str,
    plaintext: &str,
) -> Result<(), AppError> {
    let encrypted = protector.protect(plaintext.as_bytes())?;
    connection.execute(
        "INSERT INTO secure_secrets (secret_key, encrypted_value, encryption_provider, created_at, updated_at)
         VALUES (?1, ?2, ?3, strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now'))
         ON CONFLICT(secret_key) DO UPDATE SET encrypted_value=excluded.encrypted_value, encryption_provider=excluded.encryption_provider, updated_at=excluded.updated_at",
        params![key, encrypted, PROVIDER_DPAPI],
    )?;
    Ok(())
}

pub fn read_secret(
    connection: &Connection,
    protector: &dyn SecretProtector,
    key: &str,
) -> Result<Option<String>, AppError> {
    let encrypted: Option<Vec<u8>> = connection
        .query_row(
            "SELECT encrypted_value FROM secure_secrets WHERE secret_key=?1",
            params![key],
            |row| row.get(0),
        )
        .optional()?;
    encrypted
        .map(|value| {
            protector.unprotect(&value).and_then(|plain| {
                String::from_utf8(plain)
                    .map_err(|_| AppError::Database("SECRET_UTF8_INVALID".to_string()))
            })
        })
        .transpose()
}

pub fn delete_secret(connection: &Connection, key: &str) -> Result<(), AppError> {
    connection.execute(
        "DELETE FROM secure_secrets WHERE secret_key=?1",
        params![key],
    )?;
    Ok(())
}

pub fn store_supabase_session(
    connection: &Connection,
    protector: &dyn SecretProtector,
    session: &SupabaseSession,
) -> Result<(), AppError> {
    let json = serde_json::to_string(session)
        .map_err(|_| AppError::Database("SUPABASE_SESSION_SERIALIZE_FAILED".to_string()))?;
    upsert_secret(connection, protector, "cloud_supabase_session", &json)
}

pub fn read_supabase_session(
    connection: &Connection,
    protector: &dyn SecretProtector,
) -> Result<Option<SupabaseSession>, AppError> {
    read_secret(connection, protector, "cloud_supabase_session")?
        .map(|json| {
            serde_json::from_str(&json)
                .map_err(|_| AppError::Database("SUPABASE_SESSION_INVALID".to_string()))
        })
        .transpose()
}

#[cfg(windows)]
fn dpapi_protect(plaintext: &[u8]) -> Result<Vec<u8>, AppError> {
    use std::ptr::null_mut;
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{
        CryptProtectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
    };
    unsafe {
        let input = CRYPT_INTEGER_BLOB {
            cbData: plaintext.len() as u32,
            pbData: plaintext.as_ptr() as *mut u8,
        };
        let mut output = CRYPT_INTEGER_BLOB {
            cbData: 0,
            pbData: null_mut(),
        };
        let ok = CryptProtectData(
            &input,
            null_mut(),
            null_mut(),
            null_mut(),
            null_mut(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        );
        if ok == 0 {
            return Err(AppError::Database("DPAPI_PROTECT_FAILED".to_string()));
        }
        let bytes = std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec();
        LocalFree(output.pbData as *mut _);
        Ok(bytes)
    }
}

#[cfg(windows)]
fn dpapi_unprotect(ciphertext: &[u8]) -> Result<Vec<u8>, AppError> {
    use std::ptr::null_mut;
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{
        CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
    };
    unsafe {
        let input = CRYPT_INTEGER_BLOB {
            cbData: ciphertext.len() as u32,
            pbData: ciphertext.as_ptr() as *mut u8,
        };
        let mut output = CRYPT_INTEGER_BLOB {
            cbData: 0,
            pbData: null_mut(),
        };
        let ok = CryptUnprotectData(
            &input,
            null_mut(),
            null_mut(),
            null_mut(),
            null_mut(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        );
        if ok == 0 {
            return Err(AppError::Database("DPAPI_UNPROTECT_FAILED".to_string()));
        }
        let bytes = std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec();
        LocalFree(output.pbData as *mut _);
        Ok(bytes)
    }
}
