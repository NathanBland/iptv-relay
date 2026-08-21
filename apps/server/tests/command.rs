use std::process::Command;

use serde_json::Value;

fn gateway() -> Command {
    Command::new(env!("CARGO_BIN_EXE_iptv-gateway"))
}

#[test]
fn versions_command_reports_gateway_and_optional_runtime_versions() {
    let output = gateway()
        .arg("versions")
        .output()
        .expect("run versions command");
    assert!(output.status.success());
    let value: Value = serde_json::from_slice(&output.stdout).expect("version JSON");
    assert!(
        value["gateway"]
            .as_str()
            .is_some_and(|version| !version.is_empty())
    );
    assert!(value.get("ffmpeg").is_some());
    assert!(value.get("vlc").is_some());
}

#[test]
fn unknown_command_returns_a_safe_error() {
    let output = gateway()
        .arg("not-a-command")
        .output()
        .expect("run invalid command");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("unknown command"));
    assert!(stderr.contains("expected serve, worker, or versions"));
}

#[test]
fn serve_stops_before_connecting_when_database_url_is_missing() {
    let output = gateway()
        .arg("serve")
        .env_remove("DATABASE_URL")
        .output()
        .expect("run serve command");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("DATABASE_URL"));
}
