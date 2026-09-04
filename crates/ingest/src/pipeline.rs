#[cfg(test)]
use crate::parse::parse_artifact;
use crate::{
    ArtifactLimits, DecodedArtifact, DownloadRequest, DownloadedArtifact, IngestError,
    IngestFormat, IngestProgress, ParsedArtifact, PgSnapshotStore, PreparedEpgChannel,
    PreparedProgramme, PreparedProviderStream, PreparedSnapshot, ProtectedEndpoint, SnapshotOwner,
    StagedRows, XtreamStreamEndpointTemplate, download_stream, unpack_artifact,
};
use bytes::Bytes;
use chrono::{DateTime, Utc};
use futures_util::StreamExt;
use iptv_domain::{EpgChannel, Programme};
use iptv_parsers::{
    Diagnostic, ParseError, ParseLimits, XmltvParseOptions, parse_m3u_visit,
    parse_xmltv_visit_with_options,
};
use reqwest::{Client, redirect::Policy};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::{
    cell::RefCell,
    collections::HashMap,
    fmt,
    io::{BufReader, Read},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::io::{AsyncWriteExt, BufWriter};
use tracing::{debug, info};
use url::Url;
use uuid::Uuid;

/// Bridges async chunk delivery to a sync `Read` interface for the parser.
///
/// The download loop sends `Bytes` chunks through a bounded tokio channel.
/// The parser task calls `blocking_recv()` in `spawn_blocking`, pulling
/// chunks as they arrive. Backpressure is natural: if the parser is slow,
/// the channel fills and the download loop yields on `send().await`.
struct ChannelReader {
    receiver: tokio::sync::mpsc::Receiver<Bytes>,
    current: Bytes,
    pos: usize,
}

impl ChannelReader {
    fn new(receiver: tokio::sync::mpsc::Receiver<Bytes>) -> Self {
        Self {
            receiver,
            current: Bytes::new(),
            pos: 0,
        }
    }
}

impl Read for ChannelReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.pos >= self.current.len() {
            if let Some(chunk) = self.receiver.blocking_recv() {
                debug!(
                    chunk_len = chunk.len(),
                    "ChannelReader: received chunk from download loop"
                );
                self.current = chunk;
                self.pos = 0;
            } else {
                debug!("ChannelReader: channel closed (EOF)");
                return Ok(0);
            }
        }
        let available = &self.current[self.pos..];
        let n = buf.len().min(available.len());
        buf[..n].copy_from_slice(&available[..n]);
        self.pos += n;
        Ok(n)
    }
}

pub trait EndpointProtector: Send + Sync {
    #[allow(clippy::missing_errors_doc)]
    fn protect(&self, endpoint: &Url) -> Result<ProtectedEndpoint, IngestError>;
}

#[allow(async_fn_in_trait)]
pub trait JobControl: Send + Sync {
    async fn checkpoint(&self, progress: &IngestProgress) -> Result<(), IngestError>;
}

#[allow(async_fn_in_trait)]
pub trait SnapshotActivator: Send + Sync {
    async fn activate(&self, snapshot: &PreparedSnapshot) -> Result<Uuid, IngestError>;
}

impl SnapshotActivator for PgSnapshotStore {
    async fn activate(&self, snapshot: &PreparedSnapshot) -> Result<Uuid, IngestError> {
        self.activate(snapshot).await
    }
}

pub struct IngestRequest {
    pub owner: SnapshotOwner,
    pub format: IngestFormat,
    pub download: DownloadRequest,
    /// IANA timezone used only when XMLTV timestamps omit an explicit offset.
    pub source_timezone: String,
    /// Secret Xtream template that creates one URL for each stream.
    pub xtream_stream_endpoint: Option<XtreamStreamEndpointTemplate>,
    /// Xtream category names keyed by provider category ID.
    pub xtream_category_names: HashMap<String, String>,
}

impl fmt::Debug for IngestRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("IngestRequest")
            .field("owner", &self.owner)
            .field("format", &self.format)
            .field("download", &self.download)
            .field("source_timezone", &self.source_timezone)
            .field("xtream_stream_endpoint", &self.xtream_stream_endpoint)
            .field("xtream_category_count", &self.xtream_category_names.len())
            .finish()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IngestResult {
    pub snapshot_id: Uuid,
    pub checksum_sha256: String,
    pub downloaded_bytes: u64,
    pub decoded_bytes: u64,
    pub records: u64,
}

#[derive(Debug)]
pub struct Ingestor<P, C, S> {
    protector: Arc<P>,
    control: C,
    store: S,
    artifact_limits: ArtifactLimits,
    parse_limits: ParseLimits,
}

impl<P, C, S> Ingestor<P, C, S>
where
    P: EndpointProtector + 'static,
    C: JobControl,
    S: SnapshotActivator,
{
    pub fn new(protector: P, control: C, store: S) -> Self {
        let artifact_limits = ArtifactLimits::default();
        Self {
            protector: Arc::new(protector),
            control,
            store,
            artifact_limits,
            parse_limits: ParseLimits {
                max_input_bytes: artifact_limits.max_decoded_bytes,
                max_records: 2_000_000,
                ..ParseLimits::default()
            },
        }
    }

    #[must_use]
    pub fn with_limits(mut self, artifact: ArtifactLimits, mut parse: ParseLimits) -> Self {
        parse.max_input_bytes = parse.max_input_bytes.min(artifact.max_decoded_bytes);
        self.artifact_limits = artifact;
        self.parse_limits = parse;
        self
    }

    #[allow(clippy::missing_errors_doc)]
    pub async fn run(&self, request: &IngestRequest) -> Result<IngestResult, IngestError> {
        info!(
            format = ?request.format,
            "ingestion started"
        );
        self.checkpoint("downloading", 0, 0, 0, 0).await?;

        // M3U and XMLTV are line/event-based formats that the parsers can
        // consume incrementally. Try to overlap download and parse by
        // feeding chunks to the parser as they arrive. If the response is
        // compressed or the format is Xtream, fall back to the sequential
        // download-then-parse path.
        if matches!(request.format, IngestFormat::M3u | IngestFormat::Xmltv)
            && let Some(result) = self.try_streaming_run(request).await?
        {
            return Ok(result);
        }

        let artifact = self.download_with_progress(&request.download).await?;
        info!(downloaded_bytes = artifact.byte_count, "download completed");
        self.run_downloaded(request, artifact).await
    }

    /// Attempts to download and parse concurrently for uncompressed M3U/XMLTV.
    ///
    /// Returns `Ok(None)` if the response is compressed and the caller should
    /// fall back to the sequential path.
    #[allow(clippy::too_many_lines)]
    async fn try_streaming_run(
        &self,
        request: &IngestRequest,
    ) -> Result<Option<IngestResult>, IngestError> {
        let download = &request.download;
        let client = Client::builder()
            .redirect(Policy::none())
            .gzip(false)
            .build()
            .map_err(|_error| {
                info!("HTTP client construction failed");
                IngestError::HttpRequest
            })?;
        let response = client
            .get(download.endpoint().clone())
            .header(reqwest::header::ACCEPT_ENCODING, "identity")
            .send()
            .await
            .map_err(|error| {
                info!(
                    timeout = error.is_timeout(),
                    connect = error.is_connect(),
                    request = error.is_request(),
                    "streaming: HTTP request failed"
                );
                IngestError::HttpRequest
            })?;
        let status = response.status();
        if !status.is_success() {
            info!(status = status.as_u16(), "streaming: non-success status");
            return Err(IngestError::HttpStatus(status.as_u16()));
        }

        // Peek at the first chunk to detect compression.
        let mut stream = response.bytes_stream();
        let first_chunk = futures_util::StreamExt::next(&mut stream)
            .await
            .ok_or(IngestError::HttpRequest)?
            .map_err(|_error| {
                info!("streaming: first chunk failed");
                IngestError::HttpRequest
            })?;

        let is_compressed = first_chunk.starts_with(&[0x1f, 0x8b])
            || first_chunk.starts_with(&[0xfd, b'7', b'z', b'X', b'Z', 0x00])
            || first_chunk.starts_with(b"PK\x03\x04");

        if is_compressed {
            debug!("streaming: response is compressed, falling back to sequential path");
            return Ok(None);
        }

        info!("streaming: download and parse will overlap");

        // Set up the parser channel and task.
        let (tx, rx) = tokio::sync::mpsc::channel::<Bytes>(256);
        let reader = ChannelReader::new(rx);

        let format = request.format;
        let owner = request.owner;
        let source_timezone = request.source_timezone.clone();
        let protector = Arc::clone(&self.protector);
        let parse_limits = self.parse_limits;
        let max_download_bytes = self.artifact_limits.max_download_bytes;
        let records_counter = Arc::new(AtomicU64::new(0));
        let counter_for_parse = Arc::clone(&records_counter);

        let parse_task = tokio::task::spawn_blocking(move || {
            streaming_parse(
                reader,
                format,
                owner,
                source_timezone,
                protector.as_ref(),
                parse_limits,
                &counter_for_parse,
            )
        });

        // Download loop: write to tempfile, hash, enforce limits, and feed
        // chunks to the parser channel. The parser runs concurrently in
        // spawn_blocking, pulling from the channel as chunks arrive.
        let file = tempfile::tempfile()?;
        let file = tokio::fs::File::from_std(file);
        let mut file = BufWriter::with_capacity(512 * 1024, file);
        let mut hasher = Sha256::new();
        let mut byte_count = 0_u64;
        let mut last_checkpoint = std::time::Instant::now();
        let stall_timeout = download.stall_timeout;
        let max_timeout = download.max_timeout;
        let start = std::time::Instant::now();

        // Feed the first chunk we already peeked at.
        let mut pending = Some(first_chunk);

        loop {
            // Check max timeout.
            if start.elapsed() >= max_timeout {
                debug!(
                    byte_count,
                    elapsed = ?start.elapsed(),
                    "streaming: exceeded max timeout"
                );
                return Err(IngestError::HttpRequest);
            }

            // Get the next chunk, either from the pending buffer or the stream.
            let chunk = if let Some(c) = pending.take() {
                c
            } else {
                let remaining = max_timeout.checked_sub(start.elapsed()).unwrap_or_default();
                let chunk_timeout = stall_timeout.min(remaining);
                match tokio::time::timeout(chunk_timeout, stream.next()).await {
                    Ok(Some(result)) => result.map_err(|_error| {
                        debug!(byte_count, "streaming: chunk failed");
                        IngestError::HttpRequest
                    })?,
                    Ok(None) => break, // EOF
                    Err(_) => {
                        debug!(byte_count, stall_timeout = ?stall_timeout, "streaming: stalled, attempting partial activation");
                        break;
                    }
                }
            };

            let chunk_len = u64::try_from(chunk.len()).unwrap_or(u64::MAX);
            byte_count =
                byte_count
                    .checked_add(chunk_len)
                    .ok_or(IngestError::DownloadTooLarge {
                        limit: max_download_bytes,
                    })?;
            if byte_count > max_download_bytes {
                return Err(IngestError::DownloadTooLarge {
                    limit: max_download_bytes,
                });
            }

            // Write to tempfile for the audit trail / checksum.
            file.write_all(&chunk).await?;
            hasher.update(&chunk);

            // Feed to parser. If the parser task died (error), send fails.
            if tx.send(chunk).await.is_err() {
                debug!("streaming: parser task ended early");
                break;
            }

            // Periodic progress checkpoint (every 2 seconds).
            if last_checkpoint.elapsed() >= Duration::from_secs(2) {
                last_checkpoint = std::time::Instant::now();
                let records = records_counter.load(Ordering::Relaxed);
                if let Err(error) = self
                    .checkpoint("downloading", byte_count, 0, records, 0)
                    .await
                {
                    debug!(error = ?error, "streaming: checkpoint failed");
                }
            }
        }

        // Close the channel to signal EOF to the parser.
        drop(tx);

        // Report download completion with records parsed so far.
        let records_so_far = records_counter.load(Ordering::Relaxed);
        let _ = self
            .checkpoint("downloaded", byte_count, 0, records_so_far, 0)
            .await;

        // Flush and finalize the tempfile (for checksum).
        file.flush().await?;
        let sha256 = format!("{:x}", hasher.finalize());

        // Wait for the parser to finish.
        let (mut snapshot, records_seen) = parse_task
            .await
            .map_err(|_| IngestError::ArtifactIo(std::io::Error::other("parse task panicked")))??;

        if records_seen == 0 {
            debug!(
                byte_count,
                "streaming: parser produced no records, cannot activate partial snapshot"
            );
            return Err(IngestError::HttpRequest);
        }

        info!(
            downloaded_bytes = byte_count,
            records_seen, "streaming: download and parse completed"
        );

        // Finalize and activate the snapshot.
        snapshot.checksum_sha256 = sha256.clone();
        snapshot.byte_count = byte_count;
        finalize_snapshot(&mut snapshot)?;

        let _ = self
            .checkpoint("parsed", byte_count, byte_count, records_seen, 0)
            .await;
        let records = snapshot.record_count;
        let _ = self
            .checkpoint("staging", byte_count, byte_count, records_seen, records)
            .await;

        let snapshot_id = self.store.activate(&snapshot).await?;
        let _ = self
            .checkpoint("activated", byte_count, byte_count, records_seen, records)
            .await;

        info!(
            records,
            downloaded_bytes = byte_count,
            decoded_bytes = byte_count,
            "ingestion completed"
        );

        Ok(Some(IngestResult {
            snapshot_id,
            checksum_sha256: sha256,
            downloaded_bytes: byte_count,
            decoded_bytes: byte_count,
            records,
        }))
    }

    /// Performs the HTTP GET, then streams the response body into a tempfile
    /// while reporting byte-count progress through the job control channel.
    async fn download_with_progress(
        &self,
        request: &DownloadRequest,
    ) -> Result<DownloadedArtifact, IngestError> {
        let client = Client::builder()
            .redirect(Policy::none())
            .gzip(false)
            .build()
            .map_err(|_error| {
                info!("HTTP client construction failed");
                IngestError::HttpRequest
            })?;
        let response = client
            .get(request.endpoint().clone())
            .header(reqwest::header::ACCEPT_ENCODING, "identity")
            .send()
            .await
            .map_err(|error| {
                info!(
                    timeout = error.is_timeout(),
                    connect = error.is_connect(),
                    request = error.is_request(),
                    stall_timeout = ?request.stall_timeout,
                    "HTTP request failed before a response was received"
                );
                IngestError::HttpRequest
            })?;
        let status = response.status();
        if !status.is_success() {
            info!(
                status = status.as_u16(),
                content_length = ?response.content_length(),
                "HTTP source returned an unsuccessful status"
            );
            return Err(IngestError::HttpStatus(status.as_u16()));
        }
        let declared_length = response.content_length();
        let extension_hint = crate::artifact::extension_hint_public(request.endpoint());
        let limits = self.artifact_limits;
        let bytes_seen = std::sync::atomic::AtomicU64::new(0);
        let download_fut = download_stream(
            response.bytes_stream(),
            declared_length,
            extension_hint,
            limits,
            request.stall_timeout,
            request.max_timeout,
            |bytes_downloaded| {
                bytes_seen.store(bytes_downloaded, std::sync::atomic::Ordering::Relaxed);
            },
        );
        tokio::pin!(download_fut);
        let mut interval = tokio::time::interval(Duration::from_secs(5));
        interval.tick().await;
        let result = loop {
            tokio::select! {
                result = &mut download_fut => break result,
                _ = interval.tick() => {
                    let bytes = bytes_seen.load(std::sync::atomic::Ordering::Relaxed);
                    if let Err(error) = self.checkpoint("downloading", bytes, 0, 0, 0).await {
                        debug!(error = ?error, "progress checkpoint failed during download");
                    }
                }
            }
        };
        // Report the final byte count before returning.
        let final_bytes = bytes_seen.load(std::sync::atomic::Ordering::Relaxed);
        let _ = self.checkpoint("downloading", final_bytes, 0, 0, 0).await;
        result
    }

    #[allow(clippy::missing_errors_doc)]
    pub async fn run_downloaded(
        &self,
        request: &IngestRequest,
        artifact: DownloadedArtifact,
    ) -> Result<IngestResult, IngestError> {
        let downloaded_bytes = artifact.byte_count;
        self.checkpoint("downloaded", downloaded_bytes, 0, 0, 0)
            .await?;
        let limits = self.artifact_limits;
        let decoded = tokio::task::spawn_blocking(move || unpack_artifact(artifact, limits))
            .await
            .map_err(|_| IngestError::ArtifactIo(std::io::Error::other("decode task failed")))??;
        self.checkpoint(
            "decoded",
            downloaded_bytes,
            decoded.decoded_byte_count,
            0,
            0,
        )
        .await?;
        let format = request.format;
        let parse_limits = self.parse_limits;
        let decoded_bytes = decoded.decoded_byte_count;
        let checksum_sha256 = decoded.sha256.clone();
        let (snapshot, records_seen) = match format {
            IngestFormat::M3u | IngestFormat::Xmltv => {
                let protector = Arc::clone(&self.protector);
                let owner = request.owner;
                let source_timezone = request.source_timezone.clone();
                let checksum = checksum_sha256.clone();
                tokio::task::spawn_blocking(move || {
                    prepare_streaming_snapshot(
                        owner,
                        format,
                        source_timezone,
                        &decoded,
                        checksum,
                        downloaded_bytes,
                        protector.as_ref(),
                        parse_limits,
                    )
                })
                .await
                .map_err(|_| {
                    IngestError::ArtifactIo(std::io::Error::other("parse task failed"))
                })??
            }
            IngestFormat::Xtream(_) => {
                // Xtream's JSON parsers remain materialized, but are bounded by both
                // decoded bytes and max_records. M3U/XMLTV never take this path.
                let source_timezone = request.source_timezone.clone();
                let parsed = tokio::task::spawn_blocking(move || {
                    crate::parse_artifact_with_source_timezone(
                        &decoded,
                        format,
                        parse_limits,
                        &source_timezone,
                    )
                })
                .await
                .map_err(|_| {
                    IngestError::ArtifactIo(std::io::Error::other("parse task failed"))
                })??;
                let records_seen = parsed.stats().records_seen;
                let snapshot = prepare_snapshot(
                    request,
                    parsed,
                    checksum_sha256.clone(),
                    downloaded_bytes,
                    self.protector.as_ref(),
                )?;
                (snapshot, records_seen)
            }
        };
        self.checkpoint("parsed", downloaded_bytes, decoded_bytes, records_seen, 0)
            .await?;
        let records = snapshot.record_count;
        self.checkpoint(
            "staging",
            downloaded_bytes,
            decoded_bytes,
            records_seen,
            records,
        )
        .await?;
        let snapshot_id = self.store.activate(&snapshot).await?;
        self.checkpoint(
            "activated",
            downloaded_bytes,
            decoded_bytes,
            records_seen,
            records,
        )
        .await?;
        info!(
            records,
            downloaded_bytes, decoded_bytes, "ingestion completed"
        );
        Ok(IngestResult {
            snapshot_id,
            checksum_sha256,
            downloaded_bytes,
            decoded_bytes,
            records,
        })
    }

    async fn checkpoint(
        &self,
        phase: &str,
        downloaded_bytes: u64,
        decoded_bytes: u64,
        records_seen: u64,
        records_prepared: u64,
    ) -> Result<(), IngestError> {
        debug!(
            phase,
            downloaded_bytes, decoded_bytes, records_seen, records_prepared, "ingestion checkpoint"
        );
        self.control
            .checkpoint(&IngestProgress {
                phase: phase.to_owned(),
                downloaded_bytes,
                decoded_bytes,
                records_seen,
                records_prepared,
            })
            .await
    }
}

/// Parses M3U/XMLTV from a `ChannelReader` that is fed concurrently by the
/// download loop. This is the core of the streaming parse-during-download
/// optimization: the parser processes records as chunks arrive, rather than
/// waiting for the full download to complete.
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn streaming_parse<P: EndpointProtector>(
    reader: ChannelReader,
    format: IngestFormat,
    owner: SnapshotOwner,
    source_timezone: String,
    protector: &P,
    limits: ParseLimits,
    records_counter: &AtomicU64,
) -> Result<(PreparedSnapshot, u64), IngestError> {
    // For streaming, we don't know the total size ahead of time. Use the
    // configured limit as the cap; the download loop enforces it too.
    let mut snapshot = empty_snapshot(owner, format, String::new(), 0);
    let records_seen = match format {
        IngestFormat::M3u => {
            let mut callback_error = None;
            let mut stable_keys = HashMap::new();
            let summary = parse_m3u_visit(BufReader::new(reader), limits, |entry| {
                if let Err(error) =
                    stage_m3u_entry(&mut snapshot, entry, protector, &mut stable_keys)
                {
                    callback_error = Some(error);
                    return Err(ParseError::Io(std::io::Error::other(
                        "M3U staging visitor failed",
                    )));
                }
                let count = records_counter.fetch_add(1, Ordering::Relaxed) + 1;
                if count.is_multiple_of(1000) {
                    debug!(
                        records_staged = count,
                        "streaming_parse: M3U staging progress"
                    );
                }
                Ok(())
            });
            let summary = match summary {
                Ok(summary) => summary,
                Err(_) if callback_error.is_some() => {
                    return Err(callback_error.expect("checked callback error"));
                }
                Err(source) => {
                    return Err(IngestError::Parse {
                        format: "M3U",
                        source,
                    });
                }
            };
            snapshot.diagnostic_count = summary.stats.warnings + summary.stats.errors;
            snapshot.diagnostics = diagnostics_json(&summary.diagnostics);
            summary.stats.records_seen
        }
        IngestFormat::Xmltv => {
            let state = RefCell::new(XmlStreamState::new());
            let options = XmltvParseOptions {
                default_timezone: source_timezone,
            };
            let summary = parse_xmltv_visit_with_options(
                BufReader::new(reader),
                limits,
                &options,
                |channel| {
                    state.borrow_mut().merge_channel(channel);
                    records_counter.fetch_add(1, Ordering::Relaxed);
                    Ok(())
                },
                |programme| {
                    let mut state = state.borrow_mut();
                    if state.error.is_some() {
                        return Err(ParseError::Io(std::io::Error::other(
                            "XMLTV staging visitor failed",
                        )));
                    }
                    let channel_id = state.channel_id(&programme.channel_id);
                    if let Err(error) = state
                        .programmes
                        .push(prepared_programme(programme, channel_id))
                    {
                        state.error = Some(error);
                        return Err(ParseError::Io(std::io::Error::other(
                            "XMLTV staging visitor failed",
                        )));
                    }
                    records_counter.fetch_add(1, Ordering::Relaxed);
                    Ok(())
                },
            );
            let mut state = state.into_inner();
            let summary = match summary {
                Ok(summary) => summary,
                Err(_) if state.error.is_some() => {
                    return Err(state.error.take().expect("checked callback error"));
                }
                Err(source) => {
                    return Err(IngestError::Parse {
                        format: "XMLTV",
                        source,
                    });
                }
            };
            let channels = std::mem::take(&mut state.channels);
            for channel in channels {
                let id = state.channel_id(&channel.id);
                snapshot
                    .epg_channels
                    .push(prepared_epg_channel(channel, id))?;
            }
            for (xmltv_id, id) in state.ids {
                if !state.declared.contains_key(&xmltv_id) {
                    snapshot.epg_channels.push(PreparedEpgChannel {
                        id,
                        xmltv_id,
                        display_names: json!([]),
                        icon_urls: json!([]),
                        metadata: json!({"undeclared": true}),
                    })?;
                }
            }
            snapshot.programmes = state.programmes;
            snapshot.diagnostic_count = summary.stats.warnings + summary.stats.errors;
            snapshot.diagnostics = diagnostics_json(&summary.diagnostics);
            summary.stats.records_seen
        }
        IngestFormat::Xtream(_) => {
            return Err(IngestError::InvalidRequest(
                "Xtream does not support streaming parse",
            ));
        }
    };
    Ok((snapshot, records_seen))
}

#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    clippy::missing_errors_doc,
    clippy::missing_panics_doc
)]
pub fn prepare_streaming_snapshot<P: EndpointProtector>(
    owner: SnapshotOwner,
    format: IngestFormat,
    source_timezone: String,
    decoded: &DecodedArtifact,
    checksum_sha256: String,
    byte_count: u64,
    protector: &P,
    mut limits: ParseLimits,
) -> Result<(PreparedSnapshot, u64), IngestError> {
    limits.max_input_bytes = limits
        .max_input_bytes
        .min(decoded.decoded_byte_count.max(1));
    let file = decoded.try_clone_file()?;
    let mut snapshot = empty_snapshot(owner, format, checksum_sha256, byte_count);
    let records_seen = match format {
        IngestFormat::M3u => {
            let mut callback_error = None;
            let mut stable_keys = HashMap::new();
            let summary =
                parse_m3u_visit(BufReader::new(file), limits, |entry| match stage_m3u_entry(
                    &mut snapshot,
                    entry,
                    protector,
                    &mut stable_keys,
                ) {
                    Ok(()) => Ok(()),
                    Err(error) => {
                        callback_error = Some(error);
                        Err(ParseError::Io(std::io::Error::other(
                            "M3U staging visitor failed",
                        )))
                    }
                });
            let summary = match summary {
                Ok(summary) => summary,
                Err(_) if callback_error.is_some() => {
                    return Err(callback_error.expect("checked callback error"));
                }
                Err(source) => {
                    return Err(IngestError::Parse {
                        format: "M3U",
                        source,
                    });
                }
            };
            snapshot.diagnostic_count = summary.stats.warnings + summary.stats.errors;
            snapshot.diagnostics = diagnostics_json(&summary.diagnostics);
            summary.stats.records_seen
        }
        IngestFormat::Xmltv => {
            let state = RefCell::new(XmlStreamState::new());
            let options = XmltvParseOptions {
                default_timezone: source_timezone,
            };
            let summary = parse_xmltv_visit_with_options(
                BufReader::new(file),
                limits,
                &options,
                |channel| {
                    state.borrow_mut().merge_channel(channel);
                    Ok(())
                },
                |programme| {
                    let mut state = state.borrow_mut();
                    if state.error.is_some() {
                        return Err(ParseError::Io(std::io::Error::other(
                            "XMLTV staging visitor failed",
                        )));
                    }
                    let channel_id = state.channel_id(&programme.channel_id);
                    if let Err(error) = state
                        .programmes
                        .push(prepared_programme(programme, channel_id))
                    {
                        state.error = Some(error);
                        return Err(ParseError::Io(std::io::Error::other(
                            "XMLTV staging visitor failed",
                        )));
                    }
                    Ok(())
                },
            );
            let mut state = state.into_inner();
            let summary = match summary {
                Ok(summary) => summary,
                Err(_) if state.error.is_some() => {
                    return Err(state.error.take().expect("checked callback error"));
                }
                Err(source) => {
                    return Err(IngestError::Parse {
                        format: "XMLTV",
                        source,
                    });
                }
            };
            let channels = std::mem::take(&mut state.channels);
            for channel in channels {
                let id = state.channel_id(&channel.id);
                snapshot
                    .epg_channels
                    .push(prepared_epg_channel(channel, id))?;
            }
            for (xmltv_id, id) in state.ids {
                if !state.declared.contains_key(&xmltv_id) {
                    snapshot.epg_channels.push(PreparedEpgChannel {
                        id,
                        xmltv_id,
                        display_names: json!([]),
                        icon_urls: json!([]),
                        metadata: json!({"undeclared": true}),
                    })?;
                }
            }
            snapshot.programmes = state.programmes;
            snapshot.diagnostic_count = summary.stats.warnings + summary.stats.errors;
            snapshot.diagnostics = diagnostics_json(&summary.diagnostics);
            summary.stats.records_seen
        }
        IngestFormat::Xtream(_) => {
            return Err(IngestError::InvalidRequest(
                "Xtream must use the bounded JSON ingestion path",
            ));
        }
    };
    finalize_snapshot(&mut snapshot)?;
    Ok((snapshot, records_seen))
}

fn empty_snapshot(
    owner: SnapshotOwner,
    format: IngestFormat,
    checksum_sha256: String,
    byte_count: u64,
) -> PreparedSnapshot {
    PreparedSnapshot {
        id: Uuid::now_v7(),
        owner,
        format,
        checksum_sha256,
        byte_count,
        record_count: 0,
        diagnostic_count: 0,
        diagnostics: Value::Array(Vec::new()),
        provider_streams: StagedRows::new(1_000),
        epg_channels: StagedRows::new(1_000),
        programmes: StagedRows::new(1_000),
    }
}

fn stage_m3u_entry<P: EndpointProtector>(
    snapshot: &mut PreparedSnapshot,
    entry: iptv_parsers::M3uEntry,
    protector: &P,
    stable_keys: &mut HashMap<String, usize>,
) -> Result<(), IngestError> {
    let stable_key = unique_m3u_key(&entry, stable_keys);
    let endpoint = protector.protect(&entry.url)?;
    let group_name = entry.group_title().map(str::to_owned);
    let tvg_id = entry.tvg_id().map(str::to_owned);
    let provider_stream_id = first_attribute(&entry.attributes, &["channel-id", "stream-id", "id"]);
    snapshot.provider_streams.push(PreparedProviderStream {
        id: Uuid::now_v7(),
        stable_key,
        provider_stream_id,
        name: entry.title,
        group_name,
        tvg_id,
        tvg_name: entry.attributes.get("tvg-name").cloned(),
        logo_url: entry
            .attributes
            .get("tvg-logo")
            .and_then(|value| safe_public_url(value)),
        channel_number: first_attribute(
            &entry.attributes,
            &["tvg-chno", "channel-number", "ch-number"],
        ),
        endpoint,
        attributes: sanitized_map(&entry.attributes),
        directives: Value::Array(
            entry
                .directives
                .iter()
                .map(|directive| {
                    json!({
                        "name": directive.name,
                        "value": directive.value.as_deref().map(sanitize_text),
                    })
                })
                .collect(),
        ),
        supported: matches!(
            entry.url.scheme(),
            "http" | "https" | "udp" | "rtp" | "rtsp"
        ),
    })
}

struct XmlStreamState {
    channels: Vec<EpgChannel>,
    declared: HashMap<String, usize>,
    ids: HashMap<String, Uuid>,
    programmes: StagedRows<PreparedProgramme>,
    error: Option<IngestError>,
}

impl XmlStreamState {
    fn new() -> Self {
        Self {
            channels: Vec::new(),
            declared: HashMap::new(),
            ids: HashMap::new(),
            programmes: StagedRows::new(1_000),
            error: None,
        }
    }

    fn merge_channel(&mut self, channel: EpgChannel) {
        if let Some(index) = self.declared.get(&channel.id).copied() {
            merge_epg_channel(&mut self.channels[index], channel);
        } else {
            self.declared
                .insert(channel.id.clone(), self.channels.len());
            self.channels.push(channel);
        }
    }

    fn channel_id(&mut self, xmltv_id: &str) -> Uuid {
        *self
            .ids
            .entry(xmltv_id.to_owned())
            .or_insert_with(Uuid::now_v7)
    }
}

fn merge_epg_channel(existing: &mut EpgChannel, duplicate: EpgChannel) {
    extend_distinct(&mut existing.display_names, duplicate.display_names);
    extend_distinct(&mut existing.icons, duplicate.icons);
    extend_distinct(&mut existing.urls, duplicate.urls);
    extend_distinct(&mut existing.extensions, duplicate.extensions);
}

fn extend_distinct<T: PartialEq>(target: &mut Vec<T>, additions: Vec<T>) {
    for item in additions {
        if !target.contains(&item) {
            target.push(item);
        }
    }
}

fn prepared_epg_channel(channel: EpgChannel, id: Uuid) -> PreparedEpgChannel {
    PreparedEpgChannel {
        id,
        xmltv_id: channel.id,
        display_names: serde_json::to_value(channel.display_names).unwrap_or_else(|_| json!([])),
        icon_urls: Value::Array(
            channel
                .icons
                .iter()
                .filter_map(|icon| safe_public_url(icon.source.as_str()))
                .map(Value::String)
                .collect(),
        ),
        metadata: json!({}),
    }
}

fn prepared_programme(programme: Programme, epg_channel_id: Uuid) -> PreparedProgramme {
    let original_start = original_timestamp(&programme.original_start, programme.start);
    let original_stop = programme
        .original_stop
        .clone()
        .or_else(|| programme.stop.map(|value| value.to_rfc3339()));
    let title = programme
        .titles
        .first()
        .map_or_else(|| "Untitled".to_owned(), |value| value.value.clone());
    PreparedProgramme {
        id: Uuid::now_v7(),
        epg_channel_id,
        starts_at: programme.start,
        stops_at: programme.stop,
        original_start,
        original_stop,
        title,
        subtitle: programme
            .sub_titles
            .first()
            .map(|value| value.value.clone()),
        description: programme
            .descriptions
            .first()
            .map(|value| sanitize_text(&value.value)),
        categories: serde_json::to_value(programme.categories).unwrap_or_else(|_| json!([])),
        metadata: json!({
            "new": programme.new,
            "previously_shown": programme.previously_shown,
            "episode_numbers": programme.episode_numbers,
        }),
    }
}

fn original_timestamp(value: &str, normalized: DateTime<Utc>) -> String {
    if value.is_empty() {
        normalized.to_rfc3339()
    } else {
        value.to_owned()
    }
}

fn finalize_snapshot(snapshot: &mut PreparedSnapshot) -> Result<(), IngestError> {
    snapshot.record_count = u64::try_from(
        snapshot.provider_streams.len() + snapshot.epg_channels.len() + snapshot.programmes.len(),
    )
    .unwrap_or(u64::MAX);
    if !snapshot.is_nonempty() {
        return Err(IngestError::EmptySnapshot {
            format: snapshot.format.display_name(),
        });
    }
    Ok(())
}

#[allow(clippy::too_many_lines)]
/// Prepares one bounded snapshot before activation.
///
/// Call this after all source responses pass validation. The caller must
/// activate the returned snapshot in one transaction.
#[allow(clippy::missing_errors_doc)]
pub fn prepare_snapshot<P: EndpointProtector>(
    request: &IngestRequest,
    parsed: ParsedArtifact,
    checksum_sha256: String,
    byte_count: u64,
    protector: &P,
) -> Result<PreparedSnapshot, IngestError> {
    let stats = parsed.stats().clone();
    let mut snapshot = empty_snapshot(request.owner, request.format, checksum_sha256, byte_count);
    snapshot.diagnostic_count = stats.warnings + stats.errors;
    match parsed {
        ParsedArtifact::M3u(document) => {
            snapshot.diagnostics = diagnostics_json(&document.diagnostics);
            let mut stable_keys = HashMap::<String, usize>::new();
            for entry in document.entries {
                let stable_key = unique_m3u_key(&entry, &mut stable_keys);
                let endpoint = protector.protect(&entry.url)?;
                let group_name = entry.group_title().map(str::to_owned);
                let tvg_id = entry.tvg_id().map(str::to_owned);
                let provider_stream_id =
                    first_attribute(&entry.attributes, &["channel-id", "stream-id", "id"]);
                snapshot.provider_streams.push(PreparedProviderStream {
                    id: Uuid::now_v7(),
                    stable_key,
                    provider_stream_id,
                    name: entry.title,
                    group_name,
                    tvg_id,
                    tvg_name: entry.attributes.get("tvg-name").cloned(),
                    logo_url: entry
                        .attributes
                        .get("tvg-logo")
                        .and_then(|value| safe_public_url(value)),
                    channel_number: first_attribute(
                        &entry.attributes,
                        &["tvg-chno", "channel-number", "ch-number"],
                    ),
                    endpoint,
                    attributes: sanitized_map(&entry.attributes),
                    directives: Value::Array(
                        entry
                            .directives
                            .iter()
                            .map(|directive| {
                                json!({
                                    "name": directive.name,
                                    "value": directive.value.as_deref().map(sanitize_text),
                                })
                            })
                            .collect(),
                    ),
                    supported: matches!(
                        entry.url.scheme(),
                        "http" | "https" | "udp" | "rtp" | "rtsp"
                    ),
                })?;
            }
        }
        ParsedArtifact::Xmltv(document) => {
            snapshot.diagnostics = diagnostics_json(&document.diagnostics);
            let mut ids = HashMap::with_capacity(document.channels.len());
            for channel in document.channels {
                let id = Uuid::now_v7();
                ids.insert(channel.id.clone(), id);
                snapshot.epg_channels.push(PreparedEpgChannel {
                    id,
                    xmltv_id: channel.id,
                    display_names: serde_json::to_value(channel.display_names)
                        .unwrap_or_else(|_| json!([])),
                    icon_urls: Value::Array(
                        channel
                            .icons
                            .iter()
                            .filter_map(|icon| safe_public_url(icon.source.as_str()))
                            .map(Value::String)
                            .collect(),
                    ),
                    metadata: json!({}),
                })?;
            }
            for programme in document.programmes {
                let Some(epg_channel_id) = ids.get(&programme.channel_id).copied() else {
                    continue;
                };
                let original_start = original_timestamp(&programme.original_start, programme.start);
                let original_stop = programme
                    .original_stop
                    .clone()
                    .or_else(|| programme.stop.map(|value| value.to_rfc3339()));
                let title = programme
                    .titles
                    .first()
                    .map_or_else(|| "Untitled".to_owned(), |value| value.value.clone());
                snapshot.programmes.push(PreparedProgramme {
                    id: Uuid::now_v7(),
                    epg_channel_id,
                    starts_at: programme.start,
                    stops_at: programme.stop,
                    original_start,
                    original_stop,
                    title,
                    subtitle: programme
                        .sub_titles
                        .first()
                        .map(|value| value.value.clone()),
                    description: programme
                        .descriptions
                        .first()
                        .map(|value| sanitize_text(&value.value)),
                    categories: serde_json::to_value(programme.categories)
                        .unwrap_or_else(|_| json!([])),
                    metadata: json!({
                        "new": programme.new,
                        "previously_shown": programme.previously_shown,
                        "episode_numbers": programme.episode_numbers,
                    }),
                })?;
            }
        }
        ParsedArtifact::XtreamStreams(document) => {
            snapshot.diagnostics = diagnostics_json(&document.diagnostics);
            let template =
                request
                    .xtream_stream_endpoint
                    .as_ref()
                    .ok_or(IngestError::InvalidRequest(
                        "Xtream live streams require a stream endpoint template",
                    ))?;
            for stream in document.records {
                let endpoint = protector.protect(&template.url_for(stream.stream_id)?)?;
                let group_name = stream.category_id.as_ref().map(|category_id| {
                    request
                        .xtream_category_names
                        .get(category_id)
                        .cloned()
                        .unwrap_or_else(|| category_id.clone())
                });
                snapshot.provider_streams.push(PreparedProviderStream {
                    id: Uuid::now_v7(),
                    stable_key: format!("xtream:{}", stream.stream_id),
                    provider_stream_id: Some(stream.stream_id.to_string()),
                    name: stream.name,
                    group_name,
                    tvg_id: stream.epg_channel_id,
                    tvg_name: None,
                    logo_url: stream
                        .icon
                        .as_ref()
                        .and_then(|url| safe_public_url(url.as_str())),
                    channel_number: stream.channel_number.map(|value| value.to_string()),
                    endpoint,
                    attributes: sanitize_json(Value::Object(
                        stream.metadata.into_iter().collect::<Map<_, _>>(),
                    )),
                    directives: json!([]),
                    supported: true,
                })?;
            }
        }
        ParsedArtifact::XtreamEpg(document) => {
            snapshot.diagnostics = diagnostics_json(&document.diagnostics);
            let mut channels = HashMap::new();
            for entry in document.records {
                let external_id =
                    entry
                        .channel_id
                        .or(entry.epg_id)
                        .ok_or(IngestError::InvalidRequest(
                            "Xtream EPG entry has no channel identity",
                        ))?;
                let epg_channel_id = if let Some(id) = channels.get(&external_id).copied() {
                    id
                } else {
                    let id = Uuid::now_v7();
                    snapshot.epg_channels.push(PreparedEpgChannel {
                        id,
                        xmltv_id: external_id.clone(),
                        display_names: json!([]),
                        icon_urls: json!([]),
                        metadata: json!({"origin": "xtream-short-epg"}),
                    })?;
                    channels.insert(external_id, id);
                    id
                };
                snapshot.programmes.push(PreparedProgramme {
                    id: Uuid::now_v7(),
                    epg_channel_id,
                    starts_at: entry.start,
                    stops_at: Some(entry.stop),
                    original_start: entry.start.to_rfc3339(),
                    original_stop: Some(entry.stop.to_rfc3339()),
                    title: sanitize_text(&entry.title),
                    subtitle: None,
                    description: Some(sanitize_text(&entry.description)),
                    categories: json!([]),
                    metadata: sanitize_json(Value::Object(
                        entry.metadata.into_iter().collect::<Map<_, _>>(),
                    )),
                })?;
            }
        }
        ParsedArtifact::XtreamAuth(_) | ParsedArtifact::XtreamCategories(_) => {
            return Err(IngestError::InvalidRequest(
                "Xtream auth/categories are validation inputs, not activatable snapshots",
            ));
        }
    }
    finalize_snapshot(&mut snapshot)?;
    Ok(snapshot)
}

fn unique_m3u_key(
    entry: &iptv_parsers::M3uEntry,
    stable_keys: &mut HashMap<String, usize>,
) -> String {
    let base_key = stable_m3u_key(entry);
    let duplicate = stable_keys.entry(base_key.clone()).or_default();
    let stable_key = if *duplicate == 0 {
        base_key
    } else {
        format!("{base_key}:duplicate-{duplicate}")
    };
    *duplicate += 1;
    stable_key
}

fn stable_m3u_key(entry: &iptv_parsers::M3uEntry) -> String {
    if let Some(tvg_id) = entry.tvg_id().filter(|value| !value.trim().is_empty()) {
        return format!("tvg:{tvg_id}");
    }
    let mut hasher = Sha256::new();
    hasher.update(entry.title.as_bytes());
    hasher.update([0]);
    hasher.update(entry.group_title().unwrap_or_default().as_bytes());
    hasher.update([0]);
    hash_credential_independent_asset(&mut hasher, &entry.url);
    format!("derived:{:x}", hasher.finalize())
}

fn hash_credential_independent_asset(hasher: &mut Sha256, endpoint: &Url) {
    hasher.update(endpoint.host_str().unwrap_or_default().as_bytes());
    hasher.update([0]);
    if let Some(port) = endpoint.port() {
        hasher.update(port.to_be_bytes());
    }
    let segments = endpoint
        .path_segments()
        .map(Iterator::collect::<Vec<_>>)
        .unwrap_or_default();
    if segments.len() >= 4
        && matches!(segments.first().copied(), Some("live" | "movie" | "series"))
        && let Some(asset) = segments.last()
    {
        hasher.update([0]);
        hasher.update(segments[0].as_bytes());
        hasher.update([0]);
        hasher.update(asset.as_bytes());
        return;
    }

    let mut redacted = endpoint.clone();
    let _ = redacted.set_username("");
    let _ = redacted.set_password(None);
    let public_pairs = endpoint
        .query_pairs()
        .filter(|(key, _)| !is_sensitive_key(key))
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect::<Vec<_>>();
    redacted.set_query(None);
    if !public_pairs.is_empty() {
        redacted.query_pairs_mut().extend_pairs(public_pairs);
    }
    hasher.update([0]);
    hasher.update(redacted.path().as_bytes());
    hasher.update([0]);
    hasher.update(redacted.query().unwrap_or_default().as_bytes());
}

fn first_attribute(
    attributes: &std::collections::BTreeMap<String, String>,
    keys: &[&str],
) -> Option<String> {
    keys.iter()
        .find_map(|key| attributes.get(*key))
        .filter(|value| !value.trim().is_empty())
        .cloned()
}

fn is_sensitive_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    [
        "password",
        "username",
        "token",
        "secret",
        "credential",
        "api_key",
        "apikey",
        "auth",
    ]
    .iter()
    .any(|needle| key.contains(needle))
}

fn has_url_credentials(url: &Url) -> bool {
    !url.username().is_empty()
        || url.password().is_some()
        || url.query_pairs().any(|(key, _)| is_sensitive_key(&key))
}

fn safe_public_url(value: &str) -> Option<String> {
    let url = Url::parse(value).ok()?;
    (!has_url_credentials(&url)).then(|| url.to_string())
}

fn sanitize_text(value: &str) -> String {
    Url::parse(value).map_or_else(
        |_| value.to_owned(),
        |url| {
            if has_url_credentials(&url) {
                "[REDACTED CREDENTIAL URL]".to_owned()
            } else {
                value.to_owned()
            }
        },
    )
}

fn sanitized_map(attributes: &std::collections::BTreeMap<String, String>) -> Value {
    Value::Object(
        attributes
            .iter()
            .map(|(key, value)| {
                let value = if is_sensitive_key(key) {
                    Value::String("[REDACTED]".to_owned())
                } else {
                    Value::String(sanitize_text(value))
                };
                (key.clone(), value)
            })
            .collect(),
    )
}

fn sanitize_json(value: Value) -> Value {
    match value {
        Value::Object(values) => Value::Object(
            values
                .into_iter()
                .map(|(key, value)| {
                    if is_sensitive_key(&key) {
                        (key, Value::String("[REDACTED]".to_owned()))
                    } else {
                        (key, sanitize_json(value))
                    }
                })
                .collect(),
        ),
        Value::Array(values) => Value::Array(values.into_iter().map(sanitize_json).collect()),
        Value::String(value) => Value::String(sanitize_text(&value)),
        other => other,
    }
}

fn diagnostics_json(diagnostics: &[Diagnostic]) -> Value {
    Value::Array(
        diagnostics
            .iter()
            .map(|diagnostic| {
                json!({
                    "severity": format!("{:?}", diagnostic.severity).to_ascii_lowercase(),
                    "code": format!("{:?}", diagnostic.code),
                    "line": diagnostic.line,
                    "byte_offset": diagnostic.byte_offset,
                })
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ArtifactLimits, DownloadedArtifact, XtreamEndpoints, XtreamPayloadKind};
    use chrono::TimeZone;
    use std::{io::Write, sync::Mutex};

    #[derive(Debug)]
    struct TestProtector;

    impl EndpointProtector for TestProtector {
        fn protect(&self, endpoint: &Url) -> Result<ProtectedEndpoint, IngestError> {
            Ok(ProtectedEndpoint {
                template: format!(
                    "{}://{}/[encrypted]",
                    endpoint.scheme(),
                    endpoint.host_str().unwrap_or("host")
                ),
                secret_ciphertext: Some(Sha256::digest(endpoint.as_str()).to_vec()),
            })
        }
    }

    #[derive(Debug)]
    struct RejectingProtector;

    impl EndpointProtector for RejectingProtector {
        fn protect(&self, _endpoint: &Url) -> Result<ProtectedEndpoint, IngestError> {
            Err(IngestError::EndpointProtection)
        }
    }

    #[derive(Debug, Default)]
    struct TestControl {
        phases: Mutex<Vec<String>>,
        cancel_at: Option<&'static str>,
    }

    impl JobControl for TestControl {
        async fn checkpoint(&self, progress: &IngestProgress) -> Result<(), IngestError> {
            self.phases
                .lock()
                .expect("lock")
                .push(progress.phase.clone());
            if self.cancel_at == Some(progress.phase.as_str()) {
                Err(IngestError::Cancelled)
            } else {
                Ok(())
            }
        }
    }

    #[derive(Debug, Default)]
    struct TestStore {
        ids: Mutex<Vec<Uuid>>,
        stable_keys: Mutex<Vec<String>>,
    }

    impl SnapshotActivator for TestStore {
        async fn activate(&self, snapshot: &PreparedSnapshot) -> Result<Uuid, IngestError> {
            self.ids.lock().expect("lock").push(snapshot.id);
            let keys = snapshot
                .provider_streams
                .batches(100)?
                .collect::<Result<Vec<_>, _>>()?
                .into_iter()
                .flatten()
                .map(|stream| stream.stable_key)
                .collect::<Vec<_>>();
            self.stable_keys.lock().expect("lock").extend(keys);
            Ok(snapshot.id)
        }
    }

    fn downloaded(bytes: &[u8]) -> DownloadedArtifact {
        let mut file = tempfile::tempfile().expect("tempfile");
        file.write_all(bytes).expect("write");
        DownloadedArtifact::from_file(file, None).expect("artifact")
    }

    fn request(format: IngestFormat) -> IngestRequest {
        IngestRequest {
            owner: match format {
                IngestFormat::Xmltv => SnapshotOwner::EpgSource(Uuid::now_v7()),
                _ => SnapshotOwner::ProviderAccount(Uuid::now_v7()),
            },
            format,
            download: DownloadRequest::new(
                Url::parse("https://user:password@example.test/source").expect("URL"),
            ),
            source_timezone: "UTC".to_owned(),
            xtream_stream_endpoint: None,
            xtream_category_names: HashMap::new(),
        }
    }

    #[tokio::test]
    async fn m3u_pipeline_protects_urls_and_reports_progress() {
        let ingestor = Ingestor::new(TestProtector, TestControl::default(), TestStore::default())
            .with_limits(ArtifactLimits::default(), ParseLimits::default());
        let result = ingestor
            .run_downloaded(
                &request(IngestFormat::M3u),
                downloaded(
                    b"#EXTM3U\n#EXTINF:-1 tvg-id=\"one\",One\nhttps://alice:secret@example.test/live?token=x\n#EXTINF:-1 tvg-id=\"one\",One duplicate\nhttps://alice:secret@example.test/live-two?token=x\n",
                ),
            )
            .await
            .expect("ingest");
        assert_eq!(result.records, 2);
        assert_eq!(
            ingestor.control.phases.lock().expect("lock").as_slice(),
            ["downloaded", "decoded", "parsed", "staging", "activated"]
        );
        assert_eq!(ingestor.store.ids.lock().expect("lock").len(), 1);
        assert_eq!(
            ingestor.store.stable_keys.lock().expect("lock").as_slice(),
            ["tvg:one", "tvg:one:duplicate-1"]
        );
    }

    #[tokio::test]
    async fn cancellation_before_staging_never_activates() {
        let ingestor = Ingestor::new(
            TestProtector,
            TestControl {
                phases: Mutex::default(),
                cancel_at: Some("parsed"),
            },
            TestStore::default(),
        );
        let error = ingestor
            .run_downloaded(
                &request(IngestFormat::M3u),
                downloaded(b"#EXTM3U\n#EXTINF:-1,One\nhttps://example.test/one\n"),
            )
            .await
            .expect_err("cancel");
        assert!(matches!(error, IngestError::Cancelled));
        assert!(ingestor.store.ids.lock().expect("lock").is_empty());
    }

    #[tokio::test]
    async fn cancellation_before_download_skips_http_request() {
        let ingestor = Ingestor::new(
            TestProtector,
            TestControl {
                phases: Mutex::default(),
                cancel_at: Some("downloading"),
            },
            TestStore::default(),
        );
        let error = ingestor
            .run(&request(IngestFormat::M3u))
            .await
            .expect_err("cancel");
        assert!(matches!(error, IngestError::Cancelled));
    }

    #[tokio::test]
    async fn pg_snapshot_activator_delegates_empty_snapshot_validation() {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://invalid@127.0.0.1/invalid")
            .expect("lazy pool");
        let store = PgSnapshotStore::new(pool);
        let snapshot = empty_snapshot(
            SnapshotOwner::ProviderAccount(Uuid::now_v7()),
            IngestFormat::M3u,
            "checksum".to_owned(),
            0,
        );
        let error = SnapshotActivator::activate(&store, &snapshot)
            .await
            .expect_err("empty snapshot");
        assert!(matches!(error, IngestError::EmptySnapshot { .. }));
    }

    #[test]
    fn sanitizers_remove_url_and_metadata_credentials() {
        assert_eq!(
            sanitize_text("https://alice:secret@example.test/live"),
            "[REDACTED CREDENTIAL URL]"
        );
        assert_eq!(
            sanitize_text("https://example.test/live"),
            "https://example.test/live"
        );
        let sanitized = sanitize_json(json!({
            "token": "secret",
            "nested": {"url": "https://example.test/live?password=secret"}
        }));
        let text = sanitized.to_string();
        assert!(!text.contains("secret"));
        assert!(text.contains("REDACTED"));
        assert_eq!(
            sanitize_json(json!(["plain", 7, true])),
            json!(["plain", 7, true])
        );
    }

    #[test]
    fn stable_keys_prefer_tvg_id_and_hash_fallback_without_plain_url() {
        let parsed = iptv_parsers::parse_m3u(
            std::io::Cursor::new(
                b"#EXTM3U\n#EXTINF:-1 tvg-id=\"one\",One\nhttps://example.test/one\n#EXTINF:-1,Two\nhttps://alice:secret@example.test/two\n",
            ),
            ParseLimits::default(),
        )
        .expect("playlist");
        assert_eq!(stable_m3u_key(&parsed.entries[0]), "tvg:one");
        let fallback = stable_m3u_key(&parsed.entries[1]);
        assert!(fallback.starts_with("derived:"));
        assert!(!fallback.contains("alice"));
        assert!(!fallback.contains("secret"));
    }

    #[test]
    fn stable_keys_survive_credential_rotation_but_distinguish_assets() {
        let source = b"#EXTM3U\n\
#EXTINF:-1 group-title=\"Sports\",Game\nhttps://old-user:old-pass@example.test/live/old-user/old-pass/41.ts?token=old\n\
#EXTINF:-1 group-title=\"Sports\",Game\nhttps://new-user:new-pass@example.test/live/new-user/new-pass/41.ts?token=new\n\
#EXTINF:-1 group-title=\"Sports\",Game\nhttps://new-user:new-pass@example.test/live/new-user/new-pass/42.ts?token=new\n";
        let parsed = iptv_parsers::parse_m3u(std::io::Cursor::new(source), ParseLimits::default())
            .expect("playlist");
        let keys = parsed
            .entries
            .iter()
            .map(stable_m3u_key)
            .collect::<Vec<_>>();
        assert_eq!(keys[0], keys[1]);
        assert_ne!(keys[1], keys[2]);
        for key in keys {
            assert!(!key.contains("user"));
            assert!(!key.contains("pass"));
            assert!(!key.contains("token"));
        }
    }

    #[test]
    fn request_debug_redacts_download_and_xtream_template() {
        let mut request = request(IngestFormat::Xtream(XtreamPayloadKind::LiveStreams));
        let endpoints = XtreamEndpoints::from_player_api_url(
            "https://example.test/player_api.php?username=user&password=top-secret",
        )
        .expect("Xtream endpoints");
        request.xtream_stream_endpoint = Some(endpoints.into_stream_template());
        let debug = format!("{request:?}");
        assert!(!debug.contains("password"));
        assert!(!debug.contains("top-secret"));
    }

    fn xtream_request(kind: XtreamPayloadKind) -> IngestRequest {
        let mut request = request(IngestFormat::Xtream(kind));
        if kind == XtreamPayloadKind::LiveStreams {
            let endpoints = XtreamEndpoints::from_player_api_url(
                "https://example.test/player_api.php?username=user&password=secret",
            )
            .expect("Xtream endpoints");
            request.xtream_stream_endpoint = Some(endpoints.into_stream_template());
            request
                .xtream_category_names
                .insert("1".to_owned(), "News".to_owned());
        }
        request
    }

    #[tokio::test]
    async fn xmltv_pipeline_stages_channels_and_programmes() {
        let ingestor = Ingestor::new(TestProtector, TestControl::default(), TestStore::default())
            .with_limits(ArtifactLimits::default(), ParseLimits::default());
        let xml = b"<?xml version=\"1.0\"?>\
<tv>\
<channel id=\"ch1\"><display-name>Channel One</display-name></channel>\
<channel id=\"ch1\"><display-name>Alt Name</display-name><icon src=\"https://example.test/icon.png\"/></channel>\
<programme start=\"20240101000000 +0000\" stop=\"20240101010000 +0000\" channel=\"ch1\">\
<title>News Show</title><sub-title>Pilot</sub-title><desc>News description</desc><category>News</category>\
</programme>\
<programme start=\"20240101010000 +0000\" stop=\"20240101020000 +0000\" channel=\"ch2\">\
<title>Undeclared</title>\
</programme>\
</tv>";
        let result = ingestor
            .run_downloaded(&request(IngestFormat::Xmltv), downloaded(xml))
            .await
            .expect("xmltv ingest");
        assert!(result.records > 0);
        assert_eq!(
            ingestor.control.phases.lock().expect("lock").as_slice(),
            ["downloaded", "decoded", "parsed", "staging", "activated"]
        );
        assert_eq!(ingestor.store.ids.lock().expect("lock").len(), 1);
    }

    #[tokio::test]
    async fn xmltv_pipeline_rejects_empty_document() {
        let ingestor = Ingestor::new(TestProtector, TestControl::default(), TestStore::default());
        let error = ingestor
            .run_downloaded(&request(IngestFormat::Xmltv), downloaded(b"<tv/>"))
            .await
            .expect_err("empty xmltv");
        assert!(matches!(error, IngestError::EmptySnapshot { .. }));
    }

    #[test]
    fn prepare_snapshot_stages_m3u_entries_and_xmltv_records() {
        let m3u = b"#EXTM3U\n#EXTINF:-1 tvg-id=\"one\" tvg-name=\"One\" tvg-logo=\"https://example.test/logo.png\" tvg-chno=\"12\" channel-id=\"stream-1\" group-title=\"News\",One\n#KODIPROP:inputstream.adaptive.manifest_type=hls\nhttps://example.test/live/one.ts\n";
        let decoded = unpack_artifact(downloaded(m3u), ArtifactLimits::default()).expect("decode");
        let parsed =
            parse_artifact(&decoded, IngestFormat::M3u, ParseLimits::default()).expect("M3U parse");
        let snapshot = prepare_snapshot(
            &request(IngestFormat::M3u),
            parsed,
            "checksum".to_owned(),
            m3u.len() as u64,
            &TestProtector,
        )
        .expect("M3U snapshot");
        assert_eq!(snapshot.record_count, 1);
        let streams = snapshot
            .provider_streams
            .batches(100)
            .expect("batches")
            .collect::<Result<Vec<_>, _>>()
            .expect("read streams")
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();
        assert_eq!(streams[0].provider_stream_id.as_deref(), Some("stream-1"));
        assert_eq!(streams[0].channel_number.as_deref(), Some("12"));
        assert_eq!(streams[0].directives[0]["name"], "KODIPROP");

        let xml = b"<tv><channel id=\"ch1\"><display-name>One</display-name><icon src=\"https://example.test/icon.png\"/></channel><programme start=\"20240101000000 +0000\" channel=\"ch1\"><title>News</title></programme></tv>";
        let decoded = unpack_artifact(downloaded(xml), ArtifactLimits::default()).expect("decode");
        let parsed = parse_artifact(&decoded, IngestFormat::Xmltv, ParseLimits::default())
            .expect("XMLTV parse");
        let snapshot = prepare_snapshot(
            &request(IngestFormat::Xmltv),
            parsed,
            "checksum".to_owned(),
            xml.len() as u64,
            &TestProtector,
        )
        .expect("XMLTV snapshot");
        assert_eq!(snapshot.record_count, 2);
    }

    #[test]
    fn prepare_snapshot_reuses_xtream_epg_channel_ids_and_supports_epg_id() {
        let payload = br#"{"epg_listings":[{"title":"Tm9uZQ==","description":"","start_timestamp":1700000000,"stop_timestamp":1700003600,"channel_id":"ch1"},{"title":"VHdv","description":"","start_timestamp":1700003600,"stop_timestamp":1700007200,"epg_id":"ch1"}]}"#;
        let decoded =
            unpack_artifact(downloaded(payload), ArtifactLimits::default()).expect("decode");
        let parsed = parse_artifact(
            &decoded,
            IngestFormat::Xtream(XtreamPayloadKind::ShortEpg),
            ParseLimits::default(),
        )
        .expect("Xtream EPG parse");
        let snapshot = prepare_snapshot(
            &xtream_request(XtreamPayloadKind::ShortEpg),
            parsed,
            "checksum".to_owned(),
            payload.len() as u64,
            &TestProtector,
        )
        .expect("Xtream EPG snapshot");
        assert_eq!(snapshot.epg_channels.len(), 1);
        assert_eq!(snapshot.programmes.len(), 2);
        assert_eq!(snapshot.record_count, 3);
    }

    #[tokio::test]
    async fn xtream_live_streams_pipeline_stages_protected_endpoints() {
        let ingestor = Ingestor::new(TestProtector, TestControl::default(), TestStore::default());
        let payload = br#"[{"stream_id":7,"name":"News","category_id":"1","epg_channel_id":"ch1","tv_icon":"https://example.test/icon.png","num":1}]"#;
        let result = ingestor
            .run_downloaded(
                &xtream_request(XtreamPayloadKind::LiveStreams),
                downloaded(payload),
            )
            .await
            .expect("xtream streams");
        assert_eq!(result.records, 1);
        let keys = ingestor.store.stable_keys.lock().expect("lock");
        assert_eq!(keys.as_slice(), ["xtream:7"]);
    }

    #[test]
    fn xtream_stream_preparation_maps_categories_and_protects_each_url() {
        let payload = br#"[{"stream_id":7,"name":"First","category_id":"1"},{"stream_id":8,"name":"Second","category_id":"unknown"}]"#;
        let decoded =
            unpack_artifact(downloaded(payload), ArtifactLimits::default()).expect("decode");
        let parsed = parse_artifact(
            &decoded,
            IngestFormat::Xtream(XtreamPayloadKind::LiveStreams),
            ParseLimits::default(),
        )
        .expect("Xtream stream parse");
        let request = xtream_request(XtreamPayloadKind::LiveStreams);
        let snapshot = prepare_snapshot(
            &request,
            parsed,
            "checksum".to_owned(),
            payload.len() as u64,
            &TestProtector,
        )
        .expect("Xtream stream snapshot");
        let streams = snapshot
            .provider_streams
            .batches(10)
            .expect("stream batches")
            .collect::<Result<Vec<_>, _>>()
            .expect("stream rows")
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();
        assert_eq!(streams[0].group_name.as_deref(), Some("News"));
        assert_eq!(streams[1].group_name.as_deref(), Some("unknown"));
        assert_ne!(
            streams[0].endpoint.secret_ciphertext,
            streams[1].endpoint.secret_ciphertext
        );
    }

    #[tokio::test]
    async fn xtream_live_streams_require_protected_template() {
        let ingestor = Ingestor::new(TestProtector, TestControl::default(), TestStore::default());
        let payload = br#"[{"stream_id":7,"name":"News"}]"#;
        let error = ingestor
            .run_downloaded(
                &request(IngestFormat::Xtream(XtreamPayloadKind::LiveStreams)),
                downloaded(payload),
            )
            .await
            .expect_err("missing template");
        assert!(matches!(error, IngestError::InvalidRequest(_)));
    }

    #[tokio::test]
    async fn xtream_short_epg_pipeline_stages_channels_and_programmes() {
        let ingestor = Ingestor::new(TestProtector, TestControl::default(), TestStore::default());
        let payload = br#"{"epg_listings":[{"title":"TmV3cw==","description":"RGVzY3JpcHRpb24=","start_timestamp":1700000000,"stop_timestamp":1700003600,"channel_id":"ch1"}]}"#;
        let result = ingestor
            .run_downloaded(
                &xtream_request(XtreamPayloadKind::ShortEpg),
                downloaded(payload),
            )
            .await
            .expect("xtream epg");
        assert!(result.records > 0);
    }

    #[tokio::test]
    async fn xtream_short_epg_without_channel_identity_fails() {
        let ingestor = Ingestor::new(TestProtector, TestControl::default(), TestStore::default());
        let payload = br#"{"epg_listings":[{"title":"TmV3cw==","description":"","start_timestamp":1700000000,"stop_timestamp":1700003600}]}"#;
        let error = ingestor
            .run_downloaded(
                &xtream_request(XtreamPayloadKind::ShortEpg),
                downloaded(payload),
            )
            .await
            .expect_err("no channel identity");
        assert!(matches!(error, IngestError::InvalidRequest(_)));
    }

    #[tokio::test]
    async fn xtream_auth_is_not_activatable() {
        let ingestor = Ingestor::new(TestProtector, TestControl::default(), TestStore::default());
        let payload = br#"{"user_info":{"auth":1},"server_info":{}}"#;
        let error = ingestor
            .run_downloaded(
                &xtream_request(XtreamPayloadKind::Auth),
                downloaded(payload),
            )
            .await
            .expect_err("auth not activatable");
        assert!(matches!(error, IngestError::InvalidRequest(_)));
    }

    #[tokio::test]
    async fn xtream_categories_are_not_activatable() {
        let ingestor = Ingestor::new(TestProtector, TestControl::default(), TestStore::default());
        let payload = br#"[{"category_id":"1","category_name":"News"}]"#;
        let error = ingestor
            .run_downloaded(
                &xtream_request(XtreamPayloadKind::LiveCategories),
                downloaded(payload),
            )
            .await
            .expect_err("categories not activatable");
        assert!(matches!(error, IngestError::InvalidRequest(_)));
    }

    #[test]
    fn prepare_streaming_snapshot_rejects_xtream_format() {
        let decoded = unpack_artifact(
            downloaded(br#"[{"stream_id":1,"name":"x"}]"#),
            ArtifactLimits::default(),
        )
        .expect("decode");
        let error = prepare_streaming_snapshot(
            SnapshotOwner::ProviderAccount(Uuid::now_v7()),
            IngestFormat::Xtream(XtreamPayloadKind::LiveStreams),
            "UTC".to_owned(),
            &decoded,
            "checksum".to_owned(),
            0,
            &TestProtector,
            ParseLimits::default(),
        )
        .expect_err("xtream in streaming path");
        assert!(matches!(error, IngestError::InvalidRequest(_)));
    }

    #[test]
    fn prepare_streaming_snapshot_reports_parser_and_protector_errors() {
        let malformed_m3u =
            unpack_artifact(downloaded(b"#EXTM3U\n\xff\n"), ArtifactLimits::default())
                .expect("decode");
        let error = prepare_streaming_snapshot(
            SnapshotOwner::ProviderAccount(Uuid::now_v7()),
            IngestFormat::M3u,
            "UTC".to_owned(),
            &malformed_m3u,
            "checksum".to_owned(),
            0,
            &TestProtector,
            ParseLimits::default(),
        )
        .expect_err("malformed M3U");
        assert!(matches!(error, IngestError::Parse { format: "M3U", .. }));

        let valid_m3u = unpack_artifact(
            downloaded(b"#EXTM3U\n#EXTINF:-1,One\nhttps://example.test/one\n"),
            ArtifactLimits::default(),
        )
        .expect("decode");
        let error = prepare_streaming_snapshot(
            SnapshotOwner::ProviderAccount(Uuid::now_v7()),
            IngestFormat::M3u,
            "UTC".to_owned(),
            &valid_m3u,
            "checksum".to_owned(),
            0,
            &RejectingProtector,
            ParseLimits::default(),
        )
        .expect_err("protector error");
        assert!(matches!(error, IngestError::EndpointProtection));

        let malformed_xml =
            unpack_artifact(downloaded(b"<tv"), ArtifactLimits::default()).expect("decode");
        let error = prepare_streaming_snapshot(
            SnapshotOwner::EpgSource(Uuid::now_v7()),
            IngestFormat::Xmltv,
            "UTC".to_owned(),
            &malformed_xml,
            "checksum".to_owned(),
            0,
            &TestProtector,
            ParseLimits::default(),
        )
        .expect_err("malformed XMLTV");
        assert!(matches!(
            error,
            IngestError::Parse {
                format: "XMLTV",
                ..
            }
        ));
    }

    #[test]
    fn finalize_snapshot_rejects_empty_snapshot() {
        let mut snapshot = empty_snapshot(
            SnapshotOwner::ProviderAccount(Uuid::now_v7()),
            IngestFormat::M3u,
            "checksum".to_owned(),
            0,
        );
        let error = finalize_snapshot(&mut snapshot).expect_err("empty");
        assert!(matches!(error, IngestError::EmptySnapshot { .. }));
    }

    #[test]
    fn first_attribute_returns_first_non_empty_match() {
        let mut attributes = std::collections::BTreeMap::new();
        attributes.insert("id".to_owned(), "42".to_owned());
        attributes.insert("tvg-id".to_owned(), "  ".to_owned());
        assert_eq!(first_attribute(&attributes, &["id"]), Some("42".to_owned()));
        assert_eq!(first_attribute(&attributes, &["tvg-id"]), None);
        assert_eq!(first_attribute(&attributes, &["missing"]), None);
    }

    #[test]
    fn is_sensitive_key_detects_known_secrets() {
        assert!(is_sensitive_key("password"));
        assert!(is_sensitive_key("API_KEY"));
        assert!(is_sensitive_key("my-token"));
        assert!(!is_sensitive_key("name"));
    }

    #[test]
    fn has_url_credentials_checks_user_password_and_query() {
        assert!(has_url_credentials(
            &Url::parse("https://user:pass@host/path").unwrap()
        ));
        assert!(has_url_credentials(
            &Url::parse("https://host/path?token=secret").unwrap()
        ));
        assert!(!has_url_credentials(
            &Url::parse("https://host/path").unwrap()
        ));
    }

    #[test]
    fn safe_public_url_rejects_credentials() {
        assert_eq!(safe_public_url("https://user:pass@host/path"), None);
        assert_eq!(
            safe_public_url("https://host/path"),
            Some("https://host/path".to_owned())
        );
        assert_eq!(safe_public_url("not a url"), None);
    }

    #[test]
    fn sanitized_map_redacts_sensitive_keys() {
        let mut attributes = std::collections::BTreeMap::new();
        attributes.insert("name".to_owned(), "value".to_owned());
        attributes.insert("password".to_owned(), "secret".to_owned());
        let value = sanitized_map(&attributes);
        let text = value.to_string();
        assert!(text.contains("REDACTED"));
        assert!(!text.contains("secret"));
    }

    #[test]
    fn diagnostics_json_serializes_diagnostics() {
        let diagnostics = vec![Diagnostic {
            severity: iptv_parsers::Severity::Warning,
            code: iptv_parsers::DiagnosticCode::MissingHeader,
            line: Some(3),
            byte_offset: Some(42),
            message: "missing header".to_owned(),
        }];
        let value = diagnostics_json(&diagnostics);
        let text = value.to_string();
        assert!(text.contains("warning"));
        assert!(text.contains("MissingHeader"));
        assert!(text.contains("\"line\":3"));
    }

    #[test]
    fn hash_credential_independent_asset_uses_port_when_present() {
        let mut hasher = Sha256::new();
        let endpoint = Url::parse("https://example.test:8080/path").unwrap();
        hash_credential_independent_asset(&mut hasher, &endpoint);
        let digest = format!("{:x}", hasher.finalize());
        assert!(!digest.is_empty());
    }

    #[test]
    fn extend_distinct_appends_only_new_values() {
        let mut target = vec![1, 2];
        extend_distinct(&mut target, vec![2, 3]);
        assert_eq!(target, vec![1, 2, 3]);
    }

    #[test]
    fn merge_epg_channel_combines_distinct_fields() {
        let mut existing = EpgChannel {
            id: "ch1".to_owned(),
            display_names: vec![iptv_domain::LocalizedText::new("One", None)],
            icons: Vec::new(),
            urls: Vec::new(),
            extensions: Vec::new(),
        };
        let duplicate = EpgChannel {
            id: "ch1".to_owned(),
            display_names: vec![iptv_domain::LocalizedText::new("Two", None)],
            icons: Vec::new(),
            urls: Vec::new(),
            extensions: Vec::new(),
        };
        merge_epg_channel(&mut existing, duplicate);
        assert_eq!(existing.display_names.len(), 2);
    }

    #[test]
    fn prepared_programme_uses_untitled_when_no_titles() {
        let programme = Programme {
            channel_id: "ch1".to_owned(),
            start: chrono::Utc::now(),
            stop: None,
            original_start: String::new(),
            original_stop: None,
            titles: Vec::new(),
            sub_titles: Vec::new(),
            descriptions: Vec::new(),
            categories: Vec::new(),
            episode_numbers: Vec::new(),
            icons: Vec::new(),
            ratings: Vec::new(),
            credits: iptv_domain::Credits::default(),
            previously_shown: false,
            new: false,
            extensions: Vec::new(),
        };
        let result = prepared_programme(programme, Uuid::now_v7());
        assert_eq!(result.title, "Untitled");
        assert!(result.subtitle.is_none());
        assert!(result.description.is_none());
    }

    #[test]
    fn prepared_programme_preserves_literal_xmltv_timestamps() {
        let start = chrono::Utc
            .with_ymd_and_hms(2026, 7, 1, 18, 0, 0)
            .single()
            .expect("UTC timestamp");
        let programme = Programme {
            channel_id: "ch1".to_owned(),
            start,
            stop: Some(start + chrono::Duration::hours(1)),
            original_start: "20260701120000 America/Denver".to_owned(),
            original_stop: Some("20260701130000 America/Denver".to_owned()),
            titles: Vec::new(),
            sub_titles: Vec::new(),
            descriptions: Vec::new(),
            categories: Vec::new(),
            episode_numbers: Vec::new(),
            icons: Vec::new(),
            ratings: Vec::new(),
            credits: iptv_domain::Credits::default(),
            previously_shown: false,
            new: false,
            extensions: Vec::new(),
        };
        let prepared = prepared_programme(programme, Uuid::now_v7());
        assert_eq!(prepared.starts_at, start);
        assert_eq!(prepared.original_start, "20260701120000 America/Denver");
        assert_eq!(
            prepared.original_stop.as_deref(),
            Some("20260701130000 America/Denver")
        );
    }

    #[test]
    fn prepared_epg_channel_filters_credential_urls() {
        let channel = EpgChannel {
            id: "ch1".to_owned(),
            display_names: vec![iptv_domain::LocalizedText::new("One", None)],
            icons: vec![
                iptv_domain::EpgIcon {
                    source: Url::parse("https://user:pass@host/icon.png").unwrap(),
                    width: None,
                    height: None,
                },
                iptv_domain::EpgIcon {
                    source: Url::parse("https://host/safe.png").unwrap(),
                    width: None,
                    height: None,
                },
            ],
            urls: Vec::new(),
            extensions: Vec::new(),
        };
        let result = prepared_epg_channel(channel, Uuid::now_v7());
        let icons = result.icon_urls.as_array().expect("array");
        assert_eq!(icons.len(), 1);
    }

    #[tokio::test]
    #[ignore = "requires IPTV_TEST_XMLTV_URL"]
    async fn configured_live_xmltv_source_downloads_and_parses() {
        let endpoint = std::env::var("IPTV_TEST_XMLTV_URL").expect("configured XMLTV URL");
        let mut request = request(IngestFormat::Xmltv);
        request.download = DownloadRequest::new(Url::parse(&endpoint).expect("XMLTV URL"));
        let result = Ingestor::new(TestProtector, TestControl::default(), TestStore::default())
            .run(&request)
            .await
            .expect("live XMLTV ingestion");
        assert!(result.records > 0);
    }
}
