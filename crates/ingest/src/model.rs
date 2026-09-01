use crate::IngestError;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;
use std::{
    fmt,
    fs::File,
    io::{BufRead, BufReader, Seek, SeekFrom, Write},
    marker::PhantomData,
};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum XtreamPayloadKind {
    Auth,
    LiveCategories,
    LiveStreams,
    ShortEpg,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case", tag = "format", content = "payload")]
pub enum IngestFormat {
    M3u,
    Xmltv,
    Xtream(XtreamPayloadKind),
}

impl IngestFormat {
    pub const fn snapshot_kind(self) -> &'static str {
        match self {
            Self::M3u => "m3u",
            Self::Xmltv => "xmltv",
            Self::Xtream(XtreamPayloadKind::ShortEpg) => "xtream-epg",
            Self::Xtream(_) => "xtream",
        }
    }

    pub const fn display_name(self) -> &'static str {
        match self {
            Self::M3u => "M3U",
            Self::Xmltv => "XMLTV",
            Self::Xtream(XtreamPayloadKind::Auth) => "Xtream auth",
            Self::Xtream(XtreamPayloadKind::LiveCategories) => "Xtream live categories",
            Self::Xtream(XtreamPayloadKind::LiveStreams) => "Xtream live streams",
            Self::Xtream(XtreamPayloadKind::ShortEpg) => "Xtream short EPG",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SnapshotOwner {
    ProviderAccount(Uuid),
    EpgSource(Uuid),
}

impl SnapshotOwner {
    pub const fn id(self) -> Uuid {
        match self {
            Self::ProviderAccount(id) | Self::EpgSource(id) => id,
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct IngestProgress {
    pub phase: String,
    pub downloaded_bytes: u64,
    pub decoded_bytes: u64,
    pub records_seen: u64,
    pub records_prepared: u64,
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProtectedEndpoint {
    pub template: String,
    #[serde(default, with = "optional_base64")]
    pub secret_ciphertext: Option<Vec<u8>>,
}

mod optional_base64 {
    use super::*;
    use serde::{Deserializer, Serializer};

    #[allow(clippy::ref_option)]
    pub fn serialize<S>(value: &Option<Vec<u8>>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        value
            .as_ref()
            .map(|bytes| STANDARD.encode(bytes))
            .serialize(serializer)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Option<Vec<u8>>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let encoded = Option::<String>::deserialize(deserializer)?;
        encoded
            .map(|value| STANDARD.decode(value).map_err(serde::de::Error::custom))
            .transpose()
    }
}

impl fmt::Debug for ProtectedEndpoint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProtectedEndpoint")
            .field("template", &self.template)
            .field(
                "secret_ciphertext",
                &self.secret_ciphertext.as_ref().map(std::vec::Vec::len),
            )
            .finish()
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PreparedProviderStream {
    pub id: Uuid,
    pub stable_key: String,
    pub provider_stream_id: Option<String>,
    pub name: String,
    pub group_name: Option<String>,
    pub tvg_id: Option<String>,
    pub tvg_name: Option<String>,
    pub logo_url: Option<String>,
    pub channel_number: Option<String>,
    pub endpoint: ProtectedEndpoint,
    pub attributes: Value,
    pub directives: Value,
    pub supported: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PreparedEpgChannel {
    pub id: Uuid,
    pub xmltv_id: String,
    pub display_names: Value,
    pub icon_urls: Value,
    pub metadata: Value,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PreparedProgramme {
    pub id: Uuid,
    pub epg_channel_id: Uuid,
    pub starts_at: DateTime<Utc>,
    pub stops_at: Option<DateTime<Utc>>,
    pub original_start: String,
    pub original_stop: Option<String>,
    pub title: String,
    pub subtitle: Option<String>,
    pub description: Option<String>,
    pub categories: Value,
    pub metadata: Value,
}

#[derive(Debug)]
pub struct PreparedSnapshot {
    pub id: Uuid,
    pub owner: SnapshotOwner,
    pub format: IngestFormat,
    pub checksum_sha256: String,
    pub byte_count: u64,
    pub record_count: u64,
    pub diagnostic_count: u64,
    pub diagnostics: Value,
    pub provider_streams: StagedRows<PreparedProviderStream>,
    pub epg_channels: StagedRows<PreparedEpgChannel>,
    pub programmes: StagedRows<PreparedProgramme>,
}

/// Rows retained in memory up to a bound, then written as JSON lines to an
/// anonymous tempfile. Only already-protected staging rows may be stored here.
pub struct StagedRows<T> {
    max_in_memory: usize,
    max_spool_bytes: u64,
    spool_bytes: u64,
    len: usize,
    memory: Vec<T>,
    spool: Option<File>,
}

impl<T> fmt::Debug for StagedRows<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StagedRows")
            .field("max_in_memory", &self.max_in_memory)
            .field("max_spool_bytes", &self.max_spool_bytes)
            .field("spool_bytes", &self.spool_bytes)
            .field("len", &self.len)
            .field("spooled", &self.spool.is_some())
            .finish_non_exhaustive()
    }
}

impl<T> StagedRows<T>
where
    T: Serialize,
{
    pub const DEFAULT_MAX_SPOOL_BYTES: u64 = 2 * 1024 * 1024 * 1024;

    pub fn new(max_in_memory: usize) -> Self {
        Self::with_spool_limit(max_in_memory, Self::DEFAULT_MAX_SPOOL_BYTES)
    }

    pub fn with_spool_limit(max_in_memory: usize, max_spool_bytes: u64) -> Self {
        let max_in_memory = max_in_memory.max(1);
        Self {
            max_in_memory,
            max_spool_bytes,
            spool_bytes: 0,
            len: 0,
            memory: Vec::with_capacity(max_in_memory),
            spool: None,
        }
    }

    #[allow(clippy::missing_errors_doc)]
    pub fn push(&mut self, row: T) -> Result<(), IngestError> {
        if self.spool.is_none() && self.memory.len() < self.max_in_memory {
            self.memory.push(row);
            self.len += 1;
            return Ok(());
        }
        if self.spool.is_none() {
            let mut spool = tempfile::tempfile()?;
            for buffered in self.memory.drain(..) {
                self.spool_bytes = write_json_line(
                    &mut spool,
                    &buffered,
                    self.spool_bytes,
                    self.max_spool_bytes,
                )?;
            }
            self.spool = Some(spool);
        }
        let spool = self.spool.as_mut().ok_or_else(|| {
            IngestError::ArtifactIo(std::io::Error::other("staging spool is unavailable"))
        })?;
        self.spool_bytes = write_json_line(spool, &row, self.spool_bytes, self.max_spool_bytes)?;
        self.len += 1;
        Ok(())
    }

    pub const fn len(&self) -> usize {
        self.len
    }

    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }
}

impl<T> StagedRows<T>
where
    T: Clone + DeserializeOwned,
{
    #[allow(clippy::missing_errors_doc)]
    pub fn batches(&self, batch_size: usize) -> Result<StagedBatchReader<T>, IngestError> {
        if let Some(spool) = &self.spool {
            let mut file = spool.try_clone()?;
            file.seek(SeekFrom::Start(0))?;
            Ok(StagedBatchReader {
                memory: None,
                reader: Some(BufReader::new(file)),
                batch_size: batch_size.max(1),
                marker: PhantomData,
            })
        } else {
            Ok(StagedBatchReader {
                memory: Some(self.memory.clone().into_iter()),
                reader: None,
                batch_size: batch_size.max(1),
                marker: PhantomData,
            })
        }
    }
}

fn write_json_line<T: Serialize>(
    file: &mut File,
    row: &T,
    written: u64,
    limit: u64,
) -> Result<u64, IngestError> {
    let encoded = serde_json::to_vec(row).map_err(std::io::Error::other)?;
    let record_bytes = u64::try_from(encoded.len())
        .unwrap_or(u64::MAX)
        .saturating_add(1);
    let next = written
        .checked_add(record_bytes)
        .ok_or(IngestError::StagingTooLarge { limit })?;
    if next > limit {
        return Err(IngestError::StagingTooLarge { limit });
    }
    file.write_all(&encoded)?;
    file.write_all(b"\n")?;
    Ok(next)
}

#[derive(Debug)]
pub struct StagedBatchReader<T> {
    memory: Option<std::vec::IntoIter<T>>,
    reader: Option<BufReader<File>>,
    batch_size: usize,
    marker: PhantomData<T>,
}

impl<T> Iterator for StagedBatchReader<T>
where
    T: DeserializeOwned,
{
    type Item = Result<Vec<T>, IngestError>;

    fn next(&mut self) -> Option<Self::Item> {
        if let Some(memory) = self.memory.as_mut() {
            let batch = memory.by_ref().take(self.batch_size).collect::<Vec<_>>();
            return (!batch.is_empty()).then_some(Ok(batch));
        }
        let reader = self.reader.as_mut()?;
        let mut batch = Vec::with_capacity(self.batch_size);
        let mut line = String::new();
        while batch.len() < self.batch_size {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) => break,
                Ok(_) => match serde_json::from_str(line.trim_end()) {
                    Ok(row) => batch.push(row),
                    Err(error) => {
                        return Some(Err(IngestError::ArtifactIo(std::io::Error::other(error))));
                    }
                },
                Err(error) => return Some(Err(IngestError::ArtifactIo(error))),
            }
        }
        (!batch.is_empty()).then_some(Ok(batch))
    }
}

impl PreparedSnapshot {
    pub fn is_nonempty(&self) -> bool {
        !self.provider_streams.is_empty()
            || !self.epg_channels.is_empty()
            || !self.programmes.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_map_to_database_kinds() {
        assert_eq!(IngestFormat::M3u.snapshot_kind(), "m3u");
        assert_eq!(IngestFormat::Xmltv.snapshot_kind(), "xmltv");
        assert_eq!(
            IngestFormat::Xtream(XtreamPayloadKind::LiveStreams).snapshot_kind(),
            "xtream"
        );
        assert_eq!(IngestFormat::Xmltv.display_name(), "XMLTV");
    }

    #[test]
    fn protected_endpoint_debug_redacts_ciphertext() {
        let endpoint = ProtectedEndpoint {
            template: "https://example.test/{secret}".to_owned(),
            secret_ciphertext: Some(b"credential bytes".to_vec()),
        };
        let debug = format!("{endpoint:?}");
        assert!(debug.contains("Some(16)"));
        assert!(!debug.contains("credential"));
    }

    #[test]
    fn owner_id_is_independent_of_owner_type() {
        let id = Uuid::now_v7();
        assert_eq!(SnapshotOwner::ProviderAccount(id).id(), id);
        assert_eq!(SnapshotOwner::EpgSource(id).id(), id);
    }

    #[test]
    fn staged_rows_spool_after_bound_and_read_in_bounded_batches() {
        let mut rows = StagedRows::new(2);
        for value in 0_u64..7 {
            rows.push(value).expect("stage");
        }
        assert_eq!(rows.len(), 7);
        assert!(rows.spool.is_some());
        let batches = rows
            .batches(3)
            .expect("reader")
            .collect::<Result<Vec<_>, _>>()
            .expect("batches");
        assert_eq!(batches.iter().map(Vec::len).collect::<Vec<_>>(), [3, 3, 1]);
        assert_eq!(
            batches.into_iter().flatten().collect::<Vec<_>>(),
            (0..7).collect::<Vec<_>>()
        );
    }

    #[test]
    fn staged_rows_stay_in_memory_at_or_below_bound() {
        let mut rows = StagedRows::new(2);
        rows.push("one".to_owned()).expect("stage");
        rows.push("two".to_owned()).expect("stage");
        assert!(rows.spool.is_none());
        assert_eq!(
            rows.batches(8)
                .expect("batches")
                .next()
                .expect("batch")
                .expect("rows"),
            ["one", "two"]
        );
    }

    #[test]
    fn ciphertext_spool_is_compact_and_round_trips() {
        let ciphertext = vec![0xff; 16 * 1024];
        let endpoint = ProtectedEndpoint {
            template: "https://example.test/[encrypted]".to_owned(),
            secret_ciphertext: Some(ciphertext.clone()),
        };
        let mut rows = StagedRows::new(1);
        rows.push(endpoint.clone()).expect("buffer");
        rows.push(endpoint).expect("spool");
        assert!(rows.spool.is_some());
        assert!(rows.spool_bytes < u64::try_from(ciphertext.len() * 3).unwrap());
        let recovered = rows
            .batches(2)
            .expect("reader")
            .next()
            .expect("batch")
            .expect("rows");
        assert_eq!(
            recovered[0].secret_ciphertext.as_deref(),
            Some(ciphertext.as_slice())
        );
        assert_eq!(
            recovered[1].secret_ciphertext.as_deref(),
            Some(ciphertext.as_slice())
        );
    }

    #[test]
    fn spool_limit_has_a_typed_failure() {
        let mut rows = StagedRows::with_spool_limit(1, 5);
        rows.push("a".to_owned()).expect("memory row");
        assert!(matches!(
            rows.push("b".to_owned()),
            Err(IngestError::StagingTooLarge { limit: 5 })
        ));
    }
}
