use serde_json::{json, Value};
use std::env;
use std::fs;
use std::path::Path;

fn safe_google_runtime_config() -> Option<Value> {
    let client_id = env::var("BEAUTYSALOON_GOOGLE_CLIENT_ID")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            let path = Path::new("live-services.local.json");
            let text = fs::read_to_string(path).ok()?;
            serde_json::from_str::<Value>(&text)
                .ok()?
                .pointer("/google/clientId")?
                .as_str()
                .map(str::to_owned)
                .filter(|value| !value.trim().is_empty())
        })?;
    let calendar_id = env::var("BEAUTYSALOON_GOOGLE_CALENDAR_ID")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            let path = Path::new("live-services.local.json");
            let text = fs::read_to_string(path).ok()?;
            serde_json::from_str::<Value>(&text)
                .ok()?
                .pointer("/google/calendarId")?
                .as_str()
                .map(str::to_owned)
                .filter(|value| !value.trim().is_empty())
        });

    Some(json!({
        "google": {
            "clientId": client_id,
            "calendarId": calendar_id,
        }
    }))
}

fn main() {
    println!("cargo:rerun-if-env-changed=BEAUTYSALOON_GOOGLE_CLIENT_ID");
    println!("cargo:rerun-if-env-changed=BEAUTYSALOON_GOOGLE_CALENDAR_ID");
    println!("cargo:rerun-if-changed=live-services.local.json");

    let default_config = match safe_google_runtime_config() {
        Some(config) => config,
        None if env::var("PROFILE").as_deref() == Ok("release") => {
            panic!("release builds require BEAUTYSALOON_GOOGLE_CLIENT_ID or a local safe Google config")
        }
        None => json!({}),
    };
    let output = env::var("OUT_DIR").expect("OUT_DIR must be available to build BeautySaloon");
    fs::write(
        Path::new(&output).join("google-runtime-default.json"),
        serde_json::to_vec(&default_config).expect("safe Google runtime config must serialize"),
    )
    .expect("safe Google runtime config must be written");
    tauri_build::build();
}
