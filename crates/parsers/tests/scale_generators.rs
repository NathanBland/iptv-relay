//! Integration tests for the deterministic scale fixtures.
//!
//! These tests run under the `scale` cargo feature. They verify generator
//! determinism and that the generated artifacts parse back to the requested
//! record counts. They never write the full 1.16-million-entry workload.

#![cfg(feature = "scale")]

use iptv_parsers::{
    ParseLimits, parse_m3u, parse_xmltv,
    scale::{
        ScaleM3uConfig, ScaleXmltvConfig, generate_m3u, generate_xmltv, m3u_sha256, xmltv_sha256,
    },
};
use std::io::BufReader;

#[test]
fn m3u_checksum_is_stable_across_runs() {
    let config = ScaleM3uConfig {
        entries: 5_000,
        seed: ScaleM3uConfig::DEFAULT_SEED,
    };
    assert_eq!(m3u_sha256(config), m3u_sha256(config));
}

#[test]
fn xmltv_checksum_is_stable_across_runs() {
    let config = ScaleXmltvConfig {
        channels: 50,
        programmes: 1_000,
        seed: ScaleXmltvConfig::DEFAULT_SEED,
    };
    assert_eq!(xmltv_sha256(config), xmltv_sha256(config));
}

#[test]
fn generated_m3u_parses_to_the_requested_entry_count() {
    let mut buffer = Vec::new();
    generate_m3u(
        ScaleM3uConfig {
            entries: 1_000,
            seed: ScaleM3uConfig::DEFAULT_SEED,
        },
        &mut buffer,
    )
    .expect("write m3u");
    let playlist =
        parse_m3u(BufReader::new(&buffer[..]), ParseLimits::default()).expect("parse m3u");
    assert_eq!(playlist.entries.len(), 1_000);
    assert!(playlist.diagnostics.is_empty());
}

#[test]
fn generated_xmltv_parses_to_the_requested_record_count() {
    let mut buffer = Vec::new();
    generate_xmltv(
        ScaleXmltvConfig {
            channels: 25,
            programmes: 500,
            seed: ScaleXmltvConfig::DEFAULT_SEED,
        },
        &mut buffer,
    )
    .expect("write xmltv");
    let document =
        parse_xmltv(BufReader::new(&buffer[..]), ParseLimits::default()).expect("parse xmltv");
    assert_eq!(document.channels.len(), 25);
    assert_eq!(document.programmes.len(), 500);
    assert!(document.diagnostics.is_empty());
}

#[test]
fn generated_xmltv_parses_all_one_hundred_slots_per_channel_without_diagnostics() {
    let mut buffer = Vec::new();
    generate_xmltv(
        ScaleXmltvConfig {
            channels: 4,
            programmes: 400,
            seed: ScaleXmltvConfig::DEFAULT_SEED,
        },
        &mut buffer,
    )
    .expect("write xmltv");
    let document =
        parse_xmltv(BufReader::new(&buffer[..]), ParseLimits::default()).expect("parse xmltv");
    assert_eq!(document.channels.len(), 4);
    assert_eq!(document.programmes.len(), 400);
    assert!(
        document.diagnostics.is_empty(),
        "unexpected diagnostics: {:?}",
        document.diagnostics
    );
}

#[test]
fn m3u_workload_constants_match_the_task_scope() {
    assert_eq!(ScaleM3uConfig::WORKLOAD_ENTRIES, 1_160_000);
}

#[test]
fn xmltv_workload_constants_match_the_task_scope() {
    assert_eq!(ScaleXmltvConfig::WORKLOAD_CHANNELS, 2_750);
    assert_eq!(ScaleXmltvConfig::WORKLOAD_PROGRAMMES, 275_000);
}
