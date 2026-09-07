use serde_json::{json, Value};
use std::env;
use std::fs;
use std::path::Path;

fn non_empty_local_value(config: &Value, pointer: &str) -> Option<String> {
    config
        .pointer(pointer)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .filter(|value| !value.trim().is_empty())
}

fn safe_runtime_config() -> Option<Value> {
    let desktop = fs::read_to_string(".local-secrets/google-desktop-oauth.json")
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .and_then(|value| value.get("installed").cloned());
    let local_config = fs::read_to_string("live-services.local.json")
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok());
    let client_id = env::var("BEAUTYSALOON_GOOGLE_CLIENT_ID")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            desktop
                .as_ref()
                .and_then(|value| value.get("client_id")?.as_str().map(str::to_owned))
        })
        .or_else(|| {
            local_config
                .as_ref()
                .and_then(|config| non_empty_local_value(config, "/google/clientId"))
        })?;
    let calendar_id = env::var("BEAUTYSALOON_GOOGLE_CALENDAR_ID")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            local_config
                .as_ref()
                .and_then(|config| non_empty_local_value(config, "/google/calendarId"))
        });

    let client_secret = desktop
        .as_ref()
        .and_then(|value| value.get("client_secret")?.as_str())
        .filter(|value| !value.trim().is_empty())?;
    let project_url = env::var("BEAUTYSALOON_SUPABASE_PROJECT_URL")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            local_config
                .as_ref()
                .and_then(|config| non_empty_local_value(config, "/supabase/projectUrl"))
        })?;
    let publishable_key = env::var("BEAUTYSALOON_SUPABASE_PUBLISHABLE_KEY")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            local_config
                .as_ref()
                .and_then(|config| non_empty_local_value(config, "/supabase/publishableKey"))
        })?;
    Some(json!({
        "google": {
            "clientId": client_id,
            "clientSecret": client_secret,
            "calendarId": calendar_id,
        },
        "supabase": {
            "projectUrl": project_url,
            "publishableKey": publishable_key,
        },
    }))
}

fn main() {
    println!("cargo:rerun-if-env-changed=BEAUTYSALOON_GOOGLE_CLIENT_ID");
    println!("cargo:rerun-if-env-changed=BEAUTYSALOON_GOOGLE_CALENDAR_ID");
    println!("cargo:rerun-if-env-changed=BEAUTYSALOON_SUPABASE_PROJECT_URL");
    println!("cargo:rerun-if-env-changed=BEAUTYSALOON_SUPABASE_PUBLISHABLE_KEY");
    println!("cargo:rerun-if-changed=live-services.local.json");
    println!("cargo:rerun-if-changed=.local-secrets/google-desktop-oauth.json");

    let default_config = match safe_runtime_config() {
        Some(config) => config,
        None if env::var("PROFILE").as_deref() == Ok("release") => {
            panic!("release builds require safe Google and Supabase runtime config")
        }
        None => json!({}),
    };
    let output = env::var("OUT_DIR").expect("OUT_DIR must be available to build BeautySaloon");
    fs::write(
        Path::new(&output).join("runtime-default.json"),
        serde_json::to_vec(&default_config).expect("safe runtime config must serialize"),
    )
    .expect("safe runtime config must be written");
    tauri_build::build();
}
