//! Deterministic pinned-runner scale gate for the IPTV parsers and ingest
//! staging path.
//!
//! The gate generates a 1.16-million-entry M3U playlist and a 275,000-programme
//! XMLTV document from fixed seeds, parses each artifact, prepares a bounded
//! snapshot, and (when a `PostgreSQL` URL is supplied) activates the snapshot.
//! It measures parse time, staging time, activation time, and peak resident
//! set size. It enforces the task budgets and flags regressions above ten
//! percent against a pinned baseline.
//!
//! The gate writes generated artifacts to a temporary directory. It never
//! commits the full workload. Run it locally with `make scale-gate` and on the
//! pinned runner with `make scale-gate-pinned`.
//!
//! # Environment
//!
//! - `IPTV_TEST_DATABASE_URL`: optional `PostgreSQL` URL. When set, the gate
//!   activates each snapshot and measures activation time. When unset, the
//!   gate reports staging time only and marks activation as unavailable.
//! - `SCALE_GATE_ENTRIES`: M3U entry count (default 1,160,000).
//! - `SCALE_GATE_CHANNELS`: XMLTV channel count (default 2,750).
//! - `SCALE_GATE_PROGRAMMES`: XMLTV programme count (default 275,000).
//! - `SCALE_GATE_SEED_M3U`: M3U seed (default fixed).
//! - `SCALE_GATE_SEED_XMLTV`: XMLTV seed (default fixed).
//! - `SCALE_GATE_BASELINE`: path to the baseline JSON file.
//! - `SCALE_GATE_RESULTS`: path to write the results JSON file.
//! - `SCALE_GATE_LABEL`: run label (`host` or `pinned-runner`).

use std::{
    fs::File,
    io::{BufReader, Read, Write},
    path::{Path, PathBuf},
    time::Instant,
};

use anyhow::{Context, Result, bail};
use iptv_ingest::{
    ArtifactLimits, DownloadRequest, DownloadedArtifact, EndpointProtector, IngestError,
    IngestFormat, IngestRequest, PgSnapshotStore, ProtectedEndpoint, SnapshotOwner,
    parse_artifact_with_source_timezone, prepare_snapshot, unpack_artifact,
};
use iptv_parsers::{
    ParseLimits, parse_m3u, parse_xmltv,
    scale::{
        REGRESSION_TOLERANCE, ScaleBudgets, ScaleM3uConfig, ScaleXmltvConfig, exceeds_budget,
        generate_m3u, generate_xmltv, m3u_sha256, regresses, xmltv_sha256,
    },
};
use iptv_persistence::Database;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::runtime::Runtime;
use url::Url;
use uuid::Uuid;

const DEFAULT_BASELINE: &str = "tests/fixtures/scale-gate-baseline.json";
const DATABASE_URL_ENV: &str = "IPTV_TEST_DATABASE_URL";

fn main() -> Result<()> {
    let runtime = Runtime::new()?;
    runtime.block_on(async_main())
}

async fn async_main() -> Result<()> {
    let m3u_config = ScaleM3uConfig {
        entries: env_usize("SCALE_GATE_ENTRIES", ScaleM3uConfig::WORKLOAD_ENTRIES),
        seed: env_u64("SCALE_GATE_SEED_M3U", ScaleM3uConfig::DEFAULT_SEED),
    };
    let xmltv_config = ScaleXmltvConfig {
        channels: env_usize("SCALE_GATE_CHANNELS", ScaleXmltvConfig::WORKLOAD_CHANNELS),
        programmes: env_usize(
            "SCALE_GATE_PROGRAMMES",
            ScaleXmltvConfig::WORKLOAD_PROGRAMMES,
        ),
        seed: env_u64("SCALE_GATE_SEED_XMLTV", ScaleXmltvConfig::DEFAULT_SEED),
    };
    let label = std::env::var("SCALE_GATE_LABEL").unwrap_or_else(|_| "host".to_owned());
    let baseline_path = std::env::var("SCALE_GATE_BASELINE")
        .map_or_else(|_| PathBuf::from(DEFAULT_BASELINE), PathBuf::from);
    let results_path = std::env::var("SCALE_GATE_RESULTS").ok().map(PathBuf::from);
    let budgets = ScaleBudgets::default();
    let baseline = load_baseline(&baseline_path)?;

    let temp = tempfile::tempdir().context("create temp dir")?;
    let m3u_path = temp.path().join("scale.m3u");
    let xmltv_path = temp.path().join("scale.xmltv");

    let m3u_checksum = write_fixture_m3u(m3u_config, &m3u_path)?;
    let xmltv_checksum = write_fixture_xmltv(xmltv_config, &xmltv_path)?;

    let m3u_parse = measure_parse_m3u(&m3u_path)?;
    let xmltv_parse = measure_parse_xmltv(&xmltv_path)?;

    let m3u_staging = measure_staging(&m3u_path, IngestFormat::M3u, "UTC")?;
    let xmltv_staging = measure_staging(&xmltv_path, IngestFormat::Xmltv, "UTC")?;

    let database_url = std::env::var_os(DATABASE_URL_ENV);
    let activation = if let Some(url) = database_url {
        let url = url.to_string_lossy().to_string();
        Some(run_activation(&url, &m3u_path, &xmltv_path).await?)
    } else {
        None
    };

    let peak_rss_mib = peak_rss_mib();

    let report = ScaleReport {
        label: label.clone(),
        m3u_config,
        xmltv_config,
        m3u_checksum,
        xmltv_checksum,
        m3u_parse_seconds: m3u_parse.elapsed_seconds,
        m3u_entries: m3u_parse.records,
        m3u_diagnostics: m3u_parse.diagnostics,
        xmltv_parse_seconds: xmltv_parse.elapsed_seconds,
        xmltv_records: xmltv_parse.records,
        xmltv_diagnostics: xmltv_parse.diagnostics,
        m3u_staging_seconds: m3u_staging.elapsed_seconds,
        xmltv_staging_seconds: xmltv_staging.elapsed_seconds,
        m3u_activation_seconds: activation.as_ref().map(|a| a.m3u_activation_seconds),
        xmltv_activation_seconds: activation.as_ref().map(|a| a.xmltv_activation_seconds),
        peak_rss_mib,
        budgets,
        baseline: baseline.measurements,
    };

    let verdict = report.verdict();
    let json = serde_json::to_string_pretty(&report).context("serialize report")?;
    println!("{json}");
    println!("{verdict}");

    if let Some(path) = results_path {
        std::fs::write(&path, &json)
            .with_context(|| format!("write results to {}", path.display()))?;
    }

    if !verdict.passed {
        bail!("scale gate failed: {}", verdict.summary);
    }
    Ok(())
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct ScaleBaseline {
    measurements: Option<ScaleBaselineMeasurements>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize)]
struct ScaleBaselineMeasurements {
    m3u_parse_seconds: Option<f64>,
    xmltv_parse_seconds: Option<f64>,
    m3u_activation_seconds: Option<f64>,
    xmltv_activation_seconds: Option<f64>,
    peak_rss_mib: Option<f64>,
}

#[derive(Clone, Debug, Serialize)]
struct ScaleReport {
    label: String,
    m3u_config: ScaleM3uConfig,
    xmltv_config: ScaleXmltvConfig,
    m3u_checksum: String,
    xmltv_checksum: String,
    m3u_parse_seconds: f64,
    m3u_entries: usize,
    m3u_diagnostics: usize,
    xmltv_parse_seconds: f64,
    xmltv_records: usize,
    xmltv_diagnostics: usize,
    m3u_staging_seconds: f64,
    xmltv_staging_seconds: f64,
    m3u_activation_seconds: Option<f64>,
    xmltv_activation_seconds: Option<f64>,
    peak_rss_mib: Option<f64>,
    budgets: ScaleBudgets,
    baseline: Option<ScaleBaselineMeasurements>,
}

#[derive(Clone, Debug)]
struct Verdict {
    passed: bool,
    summary: String,
}

impl std::fmt::Display for Verdict {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}", self.summary)
    }
}

impl ScaleReport {
    #[allow(clippy::too_many_lines)]
    fn verdict(&self) -> Verdict {
        let mut failures: Vec<String> = Vec::new();

        let expected_m3u = self.m3u_config.entries;
        if self.m3u_entries != expected_m3u {
            failures.push(format!(
                "M3U emitted {} entries but {} were requested",
                self.m3u_entries, expected_m3u
            ));
        }
        let expected_xmltv = self
            .xmltv_config
            .channels
            .saturating_add(self.xmltv_config.programmes);
        if self.xmltv_records != expected_xmltv {
            failures.push(format!(
                "XMLTV emitted {} records but {} were requested",
                self.xmltv_records, expected_xmltv
            ));
        }
        if self.m3u_diagnostics > 0 {
            failures.push(format!(
                "M3U parser reported {} skipped or malformed records",
                self.m3u_diagnostics
            ));
        }
        if self.xmltv_diagnostics > 0 {
            failures.push(format!(
                "XMLTV parser reported {} skipped or malformed records",
                self.xmltv_diagnostics
            ));
        }

        if exceeds_budget(self.m3u_parse_seconds, self.budgets.m3u_parse_seconds) {
            failures.push(format!(
                "M3U parse {:.2}s exceeds budget {:.0}s",
                self.m3u_parse_seconds, self.budgets.m3u_parse_seconds
            ));
        }
        if exceeds_budget(self.xmltv_parse_seconds, self.budgets.xmltv_parse_seconds) {
            failures.push(format!(
                "XMLTV parse {:.2}s exceeds budget {:.0}s",
                self.xmltv_parse_seconds, self.budgets.xmltv_parse_seconds
            ));
        }
        if let Some(activation) = self.m3u_activation_seconds
            && exceeds_budget(activation, self.budgets.m3u_activation_seconds)
        {
            failures.push(format!(
                "M3U activation {:.2}s exceeds budget {:.0}s",
                activation, self.budgets.m3u_activation_seconds
            ));
        }
        if let Some(activation) = self.xmltv_activation_seconds
            && exceeds_budget(activation, self.budgets.xmltv_activation_seconds)
        {
            failures.push(format!(
                "XMLTV activation {:.2}s exceeds budget {:.0}s",
                activation, self.budgets.xmltv_activation_seconds
            ));
        }
        if let Some(rss) = self.peak_rss_mib
            && exceeds_budget(rss, self.budgets.peak_rss_mib)
        {
            failures.push(format!(
                "peak RSS {:.0} MiB exceeds budget {:.0} MiB",
                rss, self.budgets.peak_rss_mib
            ));
        }

        if let Some(baseline) = self.baseline {
            if let Some(b) = baseline.m3u_parse_seconds
                && regresses(self.m3u_parse_seconds, b, REGRESSION_TOLERANCE)
            {
                failures.push(format!(
                    "M3U parse {:.2}s regresses past baseline {:.2}s",
                    self.m3u_parse_seconds, b
                ));
            }
            if let Some(b) = baseline.xmltv_parse_seconds
                && regresses(self.xmltv_parse_seconds, b, REGRESSION_TOLERANCE)
            {
                failures.push(format!(
                    "XMLTV parse {:.2}s regresses past baseline {:.2}s",
                    self.xmltv_parse_seconds, b
                ));
            }
            if let (Some(measured), Some(b)) =
                (self.m3u_activation_seconds, baseline.m3u_activation_seconds)
                && regresses(measured, b, REGRESSION_TOLERANCE)
            {
                failures.push(format!(
                    "M3U activation {measured:.2}s regresses past baseline {b:.2}s"
                ));
            }
            if let (Some(measured), Some(b)) = (
                self.xmltv_activation_seconds,
                baseline.xmltv_activation_seconds,
            ) && regresses(measured, b, REGRESSION_TOLERANCE)
            {
                failures.push(format!(
                    "XMLTV activation {measured:.2}s regresses past baseline {b:.2}s"
                ));
            }
            if let (Some(measured), Some(b)) = (self.peak_rss_mib, baseline.peak_rss_mib)
                && regresses(measured, b, REGRESSION_TOLERANCE)
            {
                failures.push(format!(
                    "peak RSS {measured:.0} MiB regresses past baseline {b:.0} MiB"
                ));
            }
        }

        let activation_note = if self.m3u_activation_seconds.is_none() {
            "activation not measured (no database URL); pinned runner required"
        } else {
            "activation measured"
        };
        let rss_note = if self.peak_rss_mib.is_none() {
            "peak RSS unavailable on this host; pinned runner required"
        } else {
            "peak RSS measured"
        };

        let passed = failures.is_empty();
        let summary = if passed {
            format!(
                "scale gate passed (label={}); M3U parse {:.2}s, XMLTV parse {:.2}s, M3U staging {:.2}s, XMLTV staging {:.2}s; {activation_note}; {rss_note}",
                self.label,
                self.m3u_parse_seconds,
                self.xmltv_parse_seconds,
                self.m3u_staging_seconds,
                self.xmltv_staging_seconds,
            )
        } else {
            format!(
                "scale gate failed (label={}): {}; {activation_note}; {rss_note}",
                self.label,
                failures.join("; ")
            )
        };

        Verdict { passed, summary }
    }
}

#[derive(Debug)]
struct Measurement {
    elapsed_seconds: f64,
    records: usize,
    diagnostics: usize,
}

fn write_fixture_m3u(config: ScaleM3uConfig, path: &Path) -> Result<String> {
    let file = File::create(path).with_context(|| format!("create {}", path.display()))?;
    let mut writer = std::io::BufWriter::new(file);
    generate_m3u(config, &mut writer).context("generate m3u")?;
    writer.flush().context("flush m3u")?;
    Ok(m3u_sha256(config))
}

fn write_fixture_xmltv(config: ScaleXmltvConfig, path: &Path) -> Result<String> {
    let file = File::create(path).with_context(|| format!("create {}", path.display()))?;
    let mut writer = std::io::BufWriter::new(file);
    generate_xmltv(config, &mut writer).context("generate xmltv")?;
    writer.flush().context("flush xmltv")?;
    Ok(xmltv_sha256(config))
}

fn scale_limits(byte_count: u64) -> ParseLimits {
    ParseLimits {
        max_input_bytes: byte_count + 1024,
        max_line_bytes: 64 * 1024,
        max_records: 2_000_000,
        max_text_bytes: 16 * 1024 * 1024,
        max_xml_depth: 64,
    }
}

fn measure_parse_m3u(path: &Path) -> Result<Measurement> {
    let file = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let metadata = file.metadata()?;
    let limits = scale_limits(metadata.len());
    let reader = BufReader::new(file);
    let start = Instant::now();
    let playlist = parse_m3u(reader, limits).context("parse m3u")?;
    let elapsed = start.elapsed().as_secs_f64();
    Ok(Measurement {
        elapsed_seconds: elapsed,
        records: playlist.entries.len(),
        diagnostics: playlist.diagnostics.len(),
    })
}

fn measure_parse_xmltv(path: &Path) -> Result<Measurement> {
    let file = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let metadata = file.metadata()?;
    let limits = scale_limits(metadata.len());
    let reader = BufReader::new(file);
    let start = Instant::now();
    let document = parse_xmltv(reader, limits).context("parse xmltv")?;
    let elapsed = start.elapsed().as_secs_f64();
    Ok(Measurement {
        elapsed_seconds: elapsed,
        records: document.channels.len() + document.programmes.len(),
        diagnostics: document.diagnostics.len(),
    })
}

fn measure_staging(path: &Path, format: IngestFormat, timezone: &str) -> Result<Measurement> {
    let file = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let downloaded = DownloadedArtifact::from_file(file, None).context("artifact")?;
    let decoded = unpack_artifact(downloaded, ArtifactLimits::default()).context("unpack")?;
    let limits = scale_limits(decoded.decoded_byte_count);
    let start = Instant::now();
    let parsed = parse_artifact_with_source_timezone(&decoded, format, limits, timezone)
        .context("parse artifact")?;
    let snapshot = prepare_snapshot(
        &staging_request(format),
        parsed,
        decoded.sha256.clone(),
        decoded.decoded_byte_count,
        &GateProtector,
    )
    .context("prepare snapshot")?;
    let elapsed = start.elapsed().as_secs_f64();
    let records = usize::try_from(snapshot.record_count).unwrap_or(0);
    Ok(Measurement {
        elapsed_seconds: elapsed,
        records,
        diagnostics: 0,
    })
}

fn staging_request(format: IngestFormat) -> IngestRequest {
    let endpoint = Url::parse("https://fixture.example/scale").expect("static url");
    IngestRequest {
        owner: SnapshotOwner::ProviderAccount(Uuid::nil()),
        format,
        download: DownloadRequest::new(endpoint),
        source_timezone: "UTC".to_owned(),
        xtream_stream_endpoint: None,
        xtream_category_names: std::collections::HashMap::new(),
    }
}

#[derive(Debug)]
struct GateProtector;

impl EndpointProtector for GateProtector {
    fn protect(&self, endpoint: &Url) -> Result<ProtectedEndpoint, IngestError> {
        Ok(ProtectedEndpoint {
            template: "https://fixture.example/[encrypted]".to_string(),
            secret_ciphertext: Some(Sha256::digest(endpoint.as_str()).to_vec()),
        })
    }
}

#[derive(Debug)]
struct ActivationResult {
    m3u_activation_seconds: f64,
    xmltv_activation_seconds: f64,
}

async fn run_activation(
    database_url: &str,
    m3u_path: &Path,
    xmltv_path: &Path,
) -> Result<ActivationResult> {
    let database = Database::connect(database_url, 8)
        .await
        .context("connect database")?;
    database.migrate().await.context("migrate database")?;
    let store = PgSnapshotStore::new(database.pool().clone());

    let m3u_activation = activate_snapshot(m3u_path, IngestFormat::M3u, &store).await?;
    let xmltv_activation = activate_snapshot(xmltv_path, IngestFormat::Xmltv, &store).await?;

    Ok(ActivationResult {
        m3u_activation_seconds: m3u_activation,
        xmltv_activation_seconds: xmltv_activation,
    })
}

async fn activate_snapshot(
    path: &Path,
    format: IngestFormat,
    store: &PgSnapshotStore,
) -> Result<f64> {
    let file = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let downloaded = DownloadedArtifact::from_file(file, None).context("artifact")?;
    let decoded = unpack_artifact(downloaded, ArtifactLimits::default()).context("unpack")?;
    let limits = scale_limits(decoded.decoded_byte_count);
    let parsed = parse_artifact_with_source_timezone(&decoded, format, limits, "UTC")
        .context("parse artifact")?;
    let snapshot = prepare_snapshot(
        &staging_request(format),
        parsed,
        decoded.sha256.clone(),
        decoded.decoded_byte_count,
        &GateProtector,
    )
    .context("prepare snapshot")?;
    let start = Instant::now();
    store
        .activate(&snapshot)
        .await
        .context("activate snapshot")?;
    Ok(start.elapsed().as_secs_f64())
}

fn peak_rss_mib() -> Option<f64> {
    let mut content = String::new();
    let mut file = File::open("/proc/self/status").ok()?;
    file.read_to_string(&mut content).ok()?;
    for line in content.lines() {
        if let Some(rest) = line.strip_prefix("VmHWM:") {
            let kib: f64 = rest.split_whitespace().next()?.parse().ok()?;
            return Some(kib / 1024.0);
        }
    }
    None
}

fn load_baseline(path: &Path) -> Result<ScaleBaseline> {
    if !path.exists() {
        return Ok(ScaleBaseline { measurements: None });
    }
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("read baseline {}", path.display()))?;
    let baseline: ScaleBaseline = serde_json::from_str(&text)
        .with_context(|| format!("parse baseline {}", path.display()))?;
    Ok(baseline)
}

fn env_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}
