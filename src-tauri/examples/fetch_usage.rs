//! Headless verification: read the Claude Code keychain token and fetch live subscription usage.
//! Confirm output matches ~/.cache/ccstatusline/usage.json (if ccstatusline is installed).
//! Usage: cargo run --manifest-path src-tauri/Cargo.toml --example fetch_usage

use std::process::Command;

fn main() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async { run().await });
}

async fn run() {
    // Step 1: read the keychain credential JSON via the `security` CLI.
    let output = Command::new("security")
        .args([
            "find-generic-password",
            "-s",
            "Claude Code-credentials",
            "-w",
        ])
        .output()
        .expect("failed to run `security` — is this macOS with Claude Code installed?");

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        eprintln!("keychain lookup failed: {stderr}");
        eprintln!("Make sure Claude Code is installed and you have signed in.");
        std::process::exit(1);
    }

    let credentials_json = String::from_utf8(output.stdout)
        .expect("keychain output was not valid UTF-8")
        .trim()
        .to_owned();

    // Step 2: extract claudeAiOauth.accessToken from the credential JSON.
    let credentials: serde_json::Value =
        serde_json::from_str(&credentials_json).expect("keychain JSON was not valid JSON");

    let access_token = credentials
        .get("claudeAiOauth")
        .and_then(|o| o.get("accessToken"))
        .and_then(|v| v.as_str())
        .expect("claudeAiOauth.accessToken not found in keychain credential");

    println!(
        "token prefix: {}…",
        &access_token[..access_token.len().min(12)]
    );

    // Step 3: GET https://api.anthropic.com/api/oauth/usage
    let client = reqwest::Client::new();
    let resp = client
        .get("https://api.anthropic.com/api/oauth/usage")
        .header("Authorization", format!("Bearer {access_token}"))
        .header("anthropic-beta", "oauth-2025-04-20")
        .send()
        .await
        .expect("HTTP request failed");

    let status = resp.status();
    let body = resp.text().await.expect("failed to read response body");

    println!("\n--- raw response ({status}) ---");
    println!("{body}");

    if !status.is_success() {
        eprintln!("\nrequest failed with status {status}");
        std::process::exit(1);
    }

    // Step 4: parse and print field-by-field.
    let parsed: serde_json::Value =
        serde_json::from_str(&body).expect("response body was not valid JSON");

    println!("\n--- parsed fields ---");

    for bucket in &[
        "five_hour",
        "seven_day",
        "seven_day_sonnet",
        "seven_day_opus",
    ] {
        match parsed.get(*bucket) {
            None => println!("{bucket}: (absent)"),
            Some(serde_json::Value::Null) => println!("{bucket}: null"),
            Some(v) => {
                let utilization = v
                    .get("utilization")
                    .and_then(|u| u.as_f64())
                    .map(|u| format!("{u:.1}%"))
                    .unwrap_or_else(|| "null".to_owned());

                let resets_at = v.get("resets_at").map(|r| match r {
                    serde_json::Value::String(s) => s.clone(),
                    serde_json::Value::Number(n) => {
                        // epoch-seconds integer — show as-is with note
                        format!("{n} (epoch-seconds)")
                    }
                    serde_json::Value::Null => "null".to_owned(),
                    other => format!("{other}"),
                });

                println!(
                    "{bucket}: utilization={utilization}, resets_at={}",
                    resets_at.as_deref().unwrap_or("(absent)")
                );
            }
        }
    }

    match parsed.get("extra_usage") {
        None => println!("extra_usage: (absent)"),
        Some(serde_json::Value::Null) => println!("extra_usage: null (disabled)"),
        Some(v) => {
            let is_enabled = v
                .get("is_enabled")
                .and_then(|b| b.as_bool())
                .unwrap_or(false);
            let monthly_limit = v
                .get("monthly_limit")
                .and_then(|n| n.as_f64())
                .map(|n| format!("${n:.2}"))
                .unwrap_or_else(|| "null".to_owned());
            let used_credits = v
                .get("used_credits")
                .and_then(|n| n.as_f64())
                .map(|n| format!("${n:.2}"))
                .unwrap_or_else(|| "null".to_owned());
            let utilization = v
                .get("utilization")
                .and_then(|n| n.as_f64())
                .map(|n| format!("{n:.1}%"))
                .unwrap_or_else(|| "null".to_owned());
            println!(
                "extra_usage: enabled={is_enabled}, monthly_limit={monthly_limit}, \
                 used={used_credits}, utilization={utilization}"
            );
        }
    }

    // Step 5: cross-reference hint.
    let ccstatusline_path = dirs_cache_path();
    if std::path::Path::new(&ccstatusline_path).exists() {
        println!("\n--- ccstatusline cache ({ccstatusline_path}) ---");
        match std::fs::read_to_string(&ccstatusline_path) {
            Ok(contents) => println!("{contents}"),
            Err(e) => println!("(could not read: {e})"),
        }
    } else {
        println!(
            "\n(ccstatusline cache not found at {ccstatusline_path} — skipping cross-reference)"
        );
    }
}

fn dirs_cache_path() -> String {
    let home = std::env::var("HOME").unwrap_or_default();
    format!("{home}/.cache/ccstatusline/usage.json")
}
