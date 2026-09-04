use crate::IngestError;
use bytes::Bytes;
use flate2::read::GzDecoder;
use futures_util::{Stream, StreamExt, pin_mut};
use reqwest::{Client, redirect::Policy};
use sha2::{Digest, Sha256};
use std::{
    fmt,
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
    path::Path,
    time::Duration,
};
use tokio::io::{AsyncSeekExt, AsyncWriteExt};
use tracing::info;
use url::Url;
use xz2::read::XzDecoder;
use zip::ZipArchive;

/// Two gibibytes, applied independently to downloaded and decoded bytes.
pub const DEFAULT_ARTIFACT_LIMIT: u64 = 2 * 1024 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ArtifactLimits {
    pub max_download_bytes: u64,
    pub max_decoded_bytes: u64,
}

impl Default for ArtifactLimits {
    fn default() -> Self {
        Self {
            max_download_bytes: DEFAULT_ARTIFACT_LIMIT,
            max_decoded_bytes: DEFAULT_ARTIFACT_LIMIT,
        }
    }
}

pub struct DownloadRequest {
    endpoint: Url,
    /// Maximum duration to wait without receiving any new data.
    /// Resets each time a chunk arrives.
    pub stall_timeout: Duration,
    /// Overall maximum download duration regardless of progress.
    pub max_timeout: Duration,
}

impl DownloadRequest {
    pub fn new(endpoint: Url) -> Self {
        Self {
            endpoint,
            stall_timeout: Duration::from_mins(1),
            max_timeout: Duration::from_mins(10),
        }
    }

    #[must_use]
    pub fn with_timeouts(mut self, stall: Duration, max: Duration) -> Self {
        self.stall_timeout = stall;
        self.max_timeout = max;
        self
    }

    pub fn endpoint(&self) -> &Url {
        &self.endpoint
    }
}

impl fmt::Debug for DownloadRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DownloadRequest")
            .field("endpoint", &RedactedEndpoint(&self.endpoint))
            .field("stall_timeout", &self.stall_timeout)
            .field("max_timeout", &self.max_timeout)
            .finish()
    }
}

struct RedactedEndpoint<'a>(&'a Url);

impl fmt::Debug for RedactedEndpoint<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.0.scheme())?;
        formatter.write_str("://")?;
        formatter.write_str(self.0.host_str().unwrap_or("[host-redacted]"))?;
        if let Some(port) = self.0.port() {
            write!(formatter, ":{port}")?;
        }
        formatter.write_str("/[path-redacted]")
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ArchiveFormat {
    Plain,
    Gzip,
    Xz,
    Zip,
}

#[derive(Debug)]
pub struct DownloadedArtifact {
    file: File,
    pub byte_count: u64,
    pub sha256: String,
    extension_hint: Option<String>,
}

impl DownloadedArtifact {
    #[allow(clippy::missing_errors_doc)]
    pub fn from_file(mut file: File, extension_hint: Option<String>) -> Result<Self, IngestError> {
        file.seek(SeekFrom::Start(0))?;
        let mut hasher = Sha256::new();
        let byte_count = std::io::copy(&mut file, &mut hasher_writer(&mut hasher))?;
        file.seek(SeekFrom::Start(0))?;
        Ok(Self {
            file,
            byte_count,
            sha256: format!("{:x}", hasher.finalize()),
            extension_hint: extension_hint.map(|value| value.to_ascii_lowercase()),
        })
    }

    #[allow(clippy::missing_errors_doc)]
    pub fn try_clone_file(&self) -> Result<File, IngestError> {
        let mut file = self.file.try_clone()?;
        file.seek(SeekFrom::Start(0))?;
        Ok(file)
    }
}

struct HashWriter<'a>(&'a mut Sha256);

impl Write for HashWriter<'_> {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        self.0.update(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn hasher_writer(hasher: &mut Sha256) -> HashWriter<'_> {
    HashWriter(hasher)
}

#[derive(Debug)]
pub struct DecodedArtifact {
    file: File,
    pub archive_format: ArchiveFormat,
    pub decoded_byte_count: u64,
    pub downloaded_byte_count: u64,
    pub sha256: String,
}

impl DecodedArtifact {
    #[allow(clippy::missing_errors_doc)]
    pub fn try_clone_file(&self) -> Result<File, IngestError> {
        let mut file = self.file.try_clone()?;
        file.seek(SeekFrom::Start(0))?;
        Ok(file)
    }
}

/// Downloads an HTTP response without redirects or transparent content decoding.
///
/// The download uses a dynamic stall-based timeout: the timer resets each time
/// new data arrives, so a slow but steady connection will not be killed. An
/// overall `max_timeout` caps the total download duration as a safety valve.
#[allow(clippy::missing_errors_doc)]
pub async fn download_http(
    request: &DownloadRequest,
    limits: ArtifactLimits,
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
    let extension_hint = extension_hint(request.endpoint());
    download_stream(
        response.bytes_stream(),
        declared_length,
        extension_hint,
        limits,
        request.stall_timeout,
        request.max_timeout,
        |_| {},
    )
    .await
}

/// Streams chunks into an anonymous tempfile while hashing and enforcing a hard byte limit.
///
/// The `stall_timeout` resets each time a chunk arrives. If no data arrives
/// for that duration, the download fails. The `max_timeout` caps the total
/// download time regardless of progress. The `on_progress` callback is called
/// each time a chunk is written, with the cumulative byte count.
#[allow(clippy::missing_errors_doc, clippy::too_many_arguments)]
pub async fn download_stream<S, E, F>(
    stream: S,
    declared_length: Option<u64>,
    extension_hint: Option<String>,
    limits: ArtifactLimits,
    stall_timeout: Duration,
    max_timeout: Duration,
    mut on_progress: F,
) -> Result<DownloadedArtifact, IngestError>
where
    S: Stream<Item = Result<Bytes, E>>,
    F: FnMut(u64),
{
    if declared_length.is_some_and(|length| length > limits.max_download_bytes) {
        return Err(IngestError::DeclaredTooLarge {
            limit: limits.max_download_bytes,
        });
    }

    let file = tempfile::tempfile()?;
    let mut file = tokio::fs::File::from_std(file);
    let mut hasher = Sha256::new();
    let mut byte_count = 0_u64;
    let start = std::time::Instant::now();
    pin_mut!(stream);
    loop {
        let elapsed = start.elapsed();
        if elapsed >= max_timeout {
            info!(
                byte_count,
                elapsed = ?elapsed,
                max_timeout = ?max_timeout,
                "download exceeded the maximum timeout"
            );
            return Err(IngestError::HttpRequest);
        }
        let remaining = max_timeout.checked_sub(elapsed).unwrap_or_default();
        let chunk_timeout = stall_timeout.min(remaining);
        let chunk = match tokio::time::timeout(chunk_timeout, stream.next()).await {
            Ok(Some(result)) => result.map_err(|_error| {
                info!(bytes_so_far = byte_count, "download stream chunk failed");
                IngestError::HttpRequest
            })?,
            Ok(None) => break,
            Err(_) => {
                info!(
                    bytes_so_far = byte_count,
                    stall_timeout = ?stall_timeout,
                    elapsed = ?start.elapsed(),
                    "download stalled: no data received within the stall timeout"
                );
                return Err(IngestError::HttpRequest);
            }
        };
        let chunk_length = u64::try_from(chunk.len()).unwrap_or(u64::MAX);
        byte_count = byte_count
            .checked_add(chunk_length)
            .ok_or(IngestError::DownloadTooLarge {
                limit: limits.max_download_bytes,
            })?;
        if byte_count > limits.max_download_bytes {
            return Err(IngestError::DownloadTooLarge {
                limit: limits.max_download_bytes,
            });
        }
        file.write_all(&chunk).await?;
        hasher.update(&chunk);
        on_progress(byte_count);
    }
    file.flush().await?;
    file.seek(SeekFrom::Start(0)).await?;
    let file = file.into_std().await;
    Ok(DownloadedArtifact {
        file,
        byte_count,
        sha256: format!("{:x}", hasher.finalize()),
        extension_hint,
    })
}

/// Detects and decodes one archive layer into another bounded anonymous tempfile.
#[allow(clippy::missing_errors_doc)]
pub fn unpack_artifact(
    artifact: DownloadedArtifact,
    limits: ArtifactLimits,
) -> Result<DecodedArtifact, IngestError> {
    let format = detect_archive(&artifact)?;
    if format == ArchiveFormat::Plain {
        return Ok(DecodedArtifact {
            file: artifact.file,
            archive_format: format,
            decoded_byte_count: artifact.byte_count,
            downloaded_byte_count: artifact.byte_count,
            sha256: artifact.sha256,
        });
    }

    let input = artifact.try_clone_file()?;
    let mut output = tempfile::tempfile()?;
    let decoded_byte_count = match format {
        ArchiveFormat::Gzip => bounded_copy(
            &mut GzDecoder::new(input),
            &mut output,
            limits.max_decoded_bytes,
        )?,
        ArchiveFormat::Xz => bounded_copy(
            &mut XzDecoder::new(input),
            &mut output,
            limits.max_decoded_bytes,
        )?,
        ArchiveFormat::Zip => unpack_zip(input, &mut output, limits.max_decoded_bytes)?,
        ArchiveFormat::Plain => unreachable!("plain input returned above"),
    };
    output.seek(SeekFrom::Start(0))?;
    Ok(DecodedArtifact {
        file: output,
        archive_format: format,
        decoded_byte_count,
        downloaded_byte_count: artifact.byte_count,
        sha256: artifact.sha256,
    })
}

fn detect_archive(artifact: &DownloadedArtifact) -> Result<ArchiveFormat, IngestError> {
    let mut file = artifact.try_clone_file()?;
    let mut magic = [0_u8; 8];
    let count = file.read(&mut magic)?;
    let magic = &magic[..count];
    if magic.starts_with(&[0x1f, 0x8b]) {
        return Ok(ArchiveFormat::Gzip);
    }
    if magic.starts_with(&[0xfd, b'7', b'z', b'X', b'Z', 0x00]) {
        return Ok(ArchiveFormat::Xz);
    }
    if magic.starts_with(b"PK\x03\x04")
        || magic.starts_with(b"PK\x05\x06")
        || magic.starts_with(b"PK\x07\x08")
    {
        return Ok(ArchiveFormat::Zip);
    }
    Ok(match artifact.extension_hint.as_deref() {
        Some("gz" | "gzip") => ArchiveFormat::Gzip,
        Some("xz") => ArchiveFormat::Xz,
        Some("zip") => ArchiveFormat::Zip,
        _ => ArchiveFormat::Plain,
    })
}

fn bounded_copy<R: Read, W: Write>(
    reader: &mut R,
    writer: &mut W,
    limit: u64,
) -> Result<u64, IngestError> {
    let mut buffer = vec![0_u8; 64 * 1024].into_boxed_slice();
    let mut written = 0_u64;
    loop {
        let count = reader
            .read(&mut buffer)
            .map_err(|_| IngestError::InvalidArchive)?;
        if count == 0 {
            return Ok(written);
        }
        written = written
            .checked_add(u64::try_from(count).unwrap_or(u64::MAX))
            .ok_or(IngestError::DecodedTooLarge { limit })?;
        if written > limit {
            return Err(IngestError::DecodedTooLarge { limit });
        }
        writer.write_all(&buffer[..count])?;
    }
}

fn unpack_zip(input: File, output: &mut File, limit: u64) -> Result<u64, IngestError> {
    let mut archive = ZipArchive::new(input).map_err(|_| IngestError::InvalidArchive)?;
    let mut regular_index = None;
    for index in 0..archive.len() {
        let file = archive
            .by_index(index)
            .map_err(|_| IngestError::InvalidArchive)?;
        if !file.is_dir() {
            if regular_index.replace(index).is_some() {
                return Err(IngestError::AmbiguousZip);
            }
            if file.size() > limit {
                return Err(IngestError::DecodedTooLarge { limit });
            }
        }
    }
    let index = regular_index.ok_or(IngestError::AmbiguousZip)?;
    let mut file = archive
        .by_index(index)
        .map_err(|_| IngestError::InvalidArchive)?;
    bounded_copy(&mut file, output, limit)
}

pub(crate) fn extension_hint_public(endpoint: &Url) -> Option<String> {
    extension_hint(endpoint)
}

fn extension_hint(endpoint: &Url) -> Option<String> {
    Path::new(endpoint.path())
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
}

impl From<std::io::Error> for IngestError {
    fn from(error: std::io::Error) -> Self {
        Self::ArtifactIo(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::{Compression, write::GzEncoder};
    use futures_util::stream;
    use std::io::Cursor;
    use xz2::write::XzEncoder;
    use zip::{ZipWriter, write::SimpleFileOptions};

    fn artifact(bytes: &[u8], extension: Option<&str>) -> DownloadedArtifact {
        let mut file = tempfile::tempfile().expect("tempfile");
        file.write_all(bytes).expect("write");
        DownloadedArtifact::from_file(file, extension.map(str::to_owned)).expect("artifact")
    }

    fn decoded_bytes(artifact: &DecodedArtifact) -> Vec<u8> {
        let mut file = artifact.try_clone_file().expect("clone");
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).expect("read");
        bytes
    }

    #[test]
    fn request_extension_and_hash_writer_accessors_are_explicit() {
        let endpoint =
            Url::parse("https://example.test:8443/guide.XML.GZ?token=redacted").expect("endpoint");
        let request = DownloadRequest::new(endpoint.clone());
        assert_eq!(request.endpoint(), &endpoint);
        assert_eq!(extension_hint(&endpoint).as_deref(), Some("gz"));
        assert_eq!(
            extension_hint(&Url::parse("https://example.test/guide").expect("endpoint")),
            None
        );
        let mut hasher = Sha256::new();
        hasher_writer(&mut hasher).flush().expect("flush");
    }

    #[tokio::test]
    async fn streaming_download_hashes_and_rewinds_tempfile() {
        let chunks = stream::iter([
            Ok::<_, std::io::Error>(Bytes::from_static(b"abc")),
            Ok(Bytes::from_static(b"def")),
        ]);
        let downloaded = download_stream(
            chunks,
            Some(6),
            Some("m3u".into()),
            ArtifactLimits::default(),
            Duration::from_mins(1),
            Duration::from_mins(10),
            |_| {},
        )
        .await
        .expect("download");
        assert_eq!(downloaded.byte_count, 6);
        assert_eq!(
            downloaded.sha256,
            "bef57ec7f53a6d40beb640a780a639c83bc29ac8a9816f1fc6c5c6dcd93c4721"
        );
        let mut file = downloaded.try_clone_file().expect("file");
        let mut body = String::new();
        file.read_to_string(&mut body).expect("read");
        assert_eq!(body, "abcdef");
    }

    #[tokio::test]
    async fn declared_and_streamed_limits_fail_closed() {
        let limits = ArtifactLimits {
            max_download_bytes: 3,
            max_decoded_bytes: 3,
        };
        assert!(matches!(
            download_stream(
                stream::empty::<Result<Bytes, ()>>(),
                Some(4),
                None,
                limits,
                Duration::from_mins(1),
                Duration::from_mins(10),
                |_| {},
            )
            .await,
            Err(IngestError::DeclaredTooLarge { limit: 3 })
        ));
        assert!(matches!(
            download_stream(
                stream::iter([Ok::<_, ()>(Bytes::from_static(b"four"))]),
                None,
                None,
                limits,
                Duration::from_mins(1),
                Duration::from_mins(10),
                |_| {},
            )
            .await,
            Err(IngestError::DownloadTooLarge { limit: 3 })
        ));
    }

    #[tokio::test]
    async fn stream_errors_are_redacted() {
        let error = download_stream(
            stream::iter([Err::<Bytes, _>("https://user:password@example.test")]),
            None,
            None,
            ArtifactLimits::default(),
            Duration::from_mins(1),
            Duration::from_mins(10),
            |_| {},
        )
        .await
        .expect_err("network error");
        assert_eq!(error.to_string(), "HTTP request could not be completed");
        assert!(!error.to_string().contains("password"));
    }

    #[test]
    fn magic_detects_gzip_and_decodes_it() {
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(b"#EXTM3U\n").expect("write");
        let compressed = encoder.finish().expect("finish");
        let decoded = unpack_artifact(artifact(&compressed, None), ArtifactLimits::default())
            .expect("unpack");
        assert_eq!(decoded.archive_format, ArchiveFormat::Gzip);
        assert_eq!(decoded_bytes(&decoded), b"#EXTM3U\n");
    }

    #[test]
    fn extension_detects_gzip_without_magic_and_rejects_it() {
        let error = unpack_artifact(artifact(b"not gzip", Some("GZ")), ArtifactLimits::default())
            .expect_err("invalid gzip");
        assert!(matches!(error, IngestError::InvalidArchive));
    }

    #[test]
    fn magic_detects_xz_and_decodes_it() {
        let mut encoder = XzEncoder::new(Vec::new(), 3);
        encoder.write_all(b"<tv/>\n").expect("write");
        let compressed = encoder.finish().expect("finish");
        let decoded = unpack_artifact(artifact(&compressed, None), ArtifactLimits::default())
            .expect("unpack");
        assert_eq!(decoded.archive_format, ArchiveFormat::Xz);
        assert_eq!(decoded_bytes(&decoded), b"<tv/>\n");
    }

    fn zip_bytes(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut output = Cursor::new(Vec::new());
        {
            let mut writer = ZipWriter::new(&mut output);
            for (name, contents) in entries {
                writer
                    .start_file(name, SimpleFileOptions::default())
                    .expect("start file");
                writer.write_all(contents).expect("write file");
            }
            writer.finish().expect("finish");
        }
        output.into_inner()
    }

    #[test]
    fn zip_decodes_exactly_one_regular_entry() {
        let bytes = zip_bytes(&[("guide.xml", b"<tv/>\n")]);
        let decoded =
            unpack_artifact(artifact(&bytes, None), ArtifactLimits::default()).expect("unpack");
        assert_eq!(decoded.archive_format, ArchiveFormat::Zip);
        assert_eq!(decoded_bytes(&decoded), b"<tv/>\n");
    }

    #[test]
    fn zip_rejects_multiple_or_missing_regular_entries() {
        let multiple = zip_bytes(&[("one", b"1"), ("two", b"2")]);
        assert!(matches!(
            unpack_artifact(artifact(&multiple, None), ArtifactLimits::default()),
            Err(IngestError::AmbiguousZip)
        ));

        let mut output = Cursor::new(Vec::new());
        {
            let mut writer = ZipWriter::new(&mut output);
            writer
                .add_directory("empty/", SimpleFileOptions::default())
                .expect("directory");
            writer.finish().expect("finish");
        }
        assert!(matches!(
            unpack_artifact(
                artifact(&output.into_inner(), None),
                ArtifactLimits::default()
            ),
            Err(IngestError::AmbiguousZip)
        ));
    }

    #[test]
    fn decompression_limit_blocks_bombs_for_every_supported_archive() {
        let limits = ArtifactLimits {
            max_download_bytes: 1024,
            max_decoded_bytes: 3,
        };
        let mut gzip = GzEncoder::new(Vec::new(), Compression::default());
        gzip.write_all(b"four").expect("write");
        assert!(matches!(
            unpack_artifact(artifact(&gzip.finish().expect("finish"), None), limits),
            Err(IngestError::DecodedTooLarge { limit: 3 })
        ));

        let zip = zip_bytes(&[("large", b"four")]);
        assert!(matches!(
            unpack_artifact(artifact(&zip, None), limits),
            Err(IngestError::DecodedTooLarge { limit: 3 })
        ));
    }

    #[test]
    fn plain_artifacts_are_not_copied_or_modified() {
        let decoded = unpack_artifact(
            artifact(b"plain text", Some("m3u")),
            ArtifactLimits::default(),
        )
        .expect("plain");
        assert_eq!(decoded.archive_format, ArchiveFormat::Plain);
        assert_eq!(decoded.downloaded_byte_count, 10);
        assert_eq!(decoded.decoded_byte_count, 10);
        assert_eq!(decoded_bytes(&decoded), b"plain text");
    }

    #[test]
    fn request_debug_never_exposes_url_credentials_or_path() {
        let request = DownloadRequest::new(
            Url::parse("https://alice:secret@example.test/live/alice/secret?token=value")
                .expect("URL"),
        );
        let debug = format!("{request:?}");
        assert!(debug.contains("example.test"));
        for secret in ["alice", "secret", "token", "value", "/live/"] {
            assert!(!debug.contains(secret));
        }
    }

    #[test]
    fn persisted_errors_are_stable_and_redacted() {
        let errors = [
            IngestError::HttpRequest,
            IngestError::HttpStatus(401),
            IngestError::DownloadTooLarge { limit: 1 },
            IngestError::InvalidArchive,
            IngestError::EmptySnapshot { format: "M3U" },
            IngestError::EndpointProtection,
            IngestError::Cancelled,
        ];
        for error in errors {
            assert!(!error.persisted_summary().is_empty());
            assert!(!error.persisted_summary().contains("http://"));
        }
    }
}
