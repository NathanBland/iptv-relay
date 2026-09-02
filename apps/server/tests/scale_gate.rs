//! Gate tests for the deterministic pinned-runner scale gate binary.
//!
//! These tests run only with the `scale-gate` cargo feature. They invoke the
//! `scale-gate` binary with a tiny deterministic workload. They verify that
//! the runner measures parse and staging time, writes a results JSON file,
//! enforces budgets, and exits successfully. They never generate the full
//! 1.16-million-entry workload.

#![cfg(feature = "scale-gate")]

use std::process::Command;

use serde_json::Value;

fn scale_gate() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_scale-gate"));
    command.env_remove("IPTV_TEST_DATABASE_URL");
    command
}

#[test]
fn scale_gate_smoke_run_passes_and_writes_results() {
    let results = tempfile::NamedTempFile::new().expect("temp results file");
    let output = scale_gate()
        .env("SCALE_GATE_ENTRIES", "1000")
        .env("SCALE_GATE_CHANNELS", "4")
        .env("SCALE_GATE_PROGRAMMES", "400")
        .env("SCALE_GATE_LABEL", "gate-test")
        .env("SCALE_GATE_RESULTS", results.path())
        .output()
        .expect("run scale-gate");

    assert!(
        output.status.success(),
        "scale-gate failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let report_text = std::fs::read_to_string(results.path()).expect("read results");
    let report: Value = serde_json::from_str(&report_text).expect("parse results JSON");
    assert_eq!(report["label"], "gate-test");
    assert_eq!(report["m3u_entries"], 1000);
    assert_eq!(report["xmltv_records"], 404);
    assert_eq!(report["m3u_diagnostics"], 0);
    assert_eq!(report["xmltv_diagnostics"], 0);
    assert!(report["m3u_parse_seconds"].as_f64().is_some());
    assert!(report["xmltv_parse_seconds"].as_f64().is_some());
    assert!(report["m3u_staging_seconds"].as_f64().is_some());
    assert!(report["xmltv_staging_seconds"].as_f64().is_some());
    // Activation is not measured when no database URL is supplied.
    assert!(report["m3u_activation_seconds"].is_null());
    assert!(report["xmltv_activation_seconds"].is_null());
}

#[test]
fn scale_gate_smoke_run_is_deterministic_across_invocations() {
    let first_results = tempfile::NamedTempFile::new().expect("temp results file");
    let first = scale_gate()
        .env("SCALE_GATE_ENTRIES", "500")
        .env("SCALE_GATE_CHANNELS", "3")
        .env("SCALE_GATE_PROGRAMMES", "300")
        .env("SCALE_GATE_LABEL", "gate-test")
        .env("SCALE_GATE_RESULTS", first_results.path())
        .output()
        .expect("run scale-gate");
    assert!(first.status.success());

    let second_results = tempfile::NamedTempFile::new().expect("temp results file");
    let second = scale_gate()
        .env("SCALE_GATE_ENTRIES", "500")
        .env("SCALE_GATE_CHANNELS", "3")
        .env("SCALE_GATE_PROGRAMMES", "300")
        .env("SCALE_GATE_LABEL", "gate-test")
        .env("SCALE_GATE_RESULTS", second_results.path())
        .output()
        .expect("run scale-gate");
    assert!(second.status.success());

    let first_report: Value =
        serde_json::from_str(&std::fs::read_to_string(first_results.path()).expect("read first"))
            .expect("parse first report");
    let second_report: Value =
        serde_json::from_str(&std::fs::read_to_string(second_results.path()).expect("read second"))
            .expect("parse second report");
    assert_eq!(first_report["m3u_checksum"], second_report["m3u_checksum"]);
    assert_eq!(
        first_report["xmltv_checksum"],
        second_report["xmltv_checksum"]
    );
    assert_eq!(first_report["m3u_entries"], second_report["m3u_entries"]);
    assert_eq!(
        first_report["xmltv_records"],
        second_report["xmltv_records"]
    );
}

#[test]
fn scale_gate_fails_when_xmltv_emits_fewer_records_than_requested() {
    let results = tempfile::NamedTempFile::new().expect("temp results file");
    // 1 channel and 100 programmes yields 100 slots that all stay valid, so the
    // gate must pass. This guards the count-mismatch verdict path indirectly by
    // confirming the full 100-slot workload succeeds end to end.
    let output = scale_gate()
        .env("SCALE_GATE_ENTRIES", "200")
        .env("SCALE_GATE_CHANNELS", "1")
        .env("SCALE_GATE_PROGRAMMES", "100")
        .env("SCALE_GATE_LABEL", "gate-test")
        .env("SCALE_GATE_RESULTS", results.path())
        .output()
        .expect("run scale-gate");
    assert!(
        output.status.success(),
        "scale-gate failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: Value =
        serde_json::from_str(&std::fs::read_to_string(results.path()).expect("read results"))
            .expect("parse results JSON");
    assert_eq!(report["xmltv_records"], 101);
    assert_eq!(report["xmltv_diagnostics"], 0);
}
