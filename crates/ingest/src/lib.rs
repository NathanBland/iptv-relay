//! Durable, resource-bounded ingestion of IPTV catalogue and guide artifacts.

mod artifact;
mod model;
mod parse;
mod pipeline;
mod store;

pub use artifact::{
    ArchiveFormat, ArtifactLimits, DecodedArtifact, DownloadRequest, DownloadedArtifact,
    download_http, download_stream, unpack_artifact,
};
pub use model::{
    IngestFormat, IngestProgress, PreparedEpgChannel, PreparedProgramme, PreparedProviderStream,
    PreparedSnapshot, ProtectedEndpoint, SnapshotOwner, StagedRows, XtreamPayloadKind,
};
pub use parse::{ParsedArtifact, parse_artifact};
pub use pipeline::{EndpointProtector, IngestRequest, IngestResult, Ingestor, JobControl};
pub use store::PgSnapshotStore;

use thiserror::Error;

/// Errors deliberately omit source URLs, response bodies, credentials, and raw media metadata.
#[derive(Debug, Error)]
pub enum IngestError {
    #[error("HTTP request could not be completed")]
    HttpRequest,
    #[error("HTTP source returned status {0}")]
    HttpStatus(u16),
    #[error("declared artifact length exceeds the {limit}-byte limit")]
    DeclaredTooLarge { limit: u64 },
    #[error("artifact download exceeds the {limit}-byte limit")]
    DownloadTooLarge { limit: u64 },
    #[error("decoded artifact exceeds the {limit}-byte limit")]
    DecodedTooLarge { limit: u64 },
    #[error("temporary artifact I/O failed")]
    ArtifactIo(#[source] std::io::Error),
    #[error("prepared-row spool exceeds the {limit}-byte limit")]
    StagingTooLarge { limit: u64 },
    #[error("archive is malformed or unsupported")]
    InvalidArchive,
    #[error("ZIP input must contain exactly one regular file")]
    AmbiguousZip,
    #[error("artifact parse failed for {format}")]
    Parse {
        format: &'static str,
        #[source]
        source: iptv_parsers::ParseError,
    },
    #[error("parsed {format} artifact contains no usable records")]
    EmptySnapshot { format: &'static str },
    #[error("stream endpoint protection failed")]
    EndpointProtection,
    #[error("snapshot storage failed")]
    Storage(#[from] sqlx::Error),
    #[error("ingestion job was cancelled")]
    Cancelled,
    #[error("ingestion job ownership was lost")]
    OwnershipLost,
    #[error("ingestion request is invalid: {0}")]
    InvalidRequest(&'static str),
}

impl IngestError {
    /// A stable, credential-safe message suitable for job persistence.
    pub const fn persisted_summary(&self) -> &'static str {
        match self {
            Self::HttpRequest => "HTTP request failed",
            Self::HttpStatus(_) => "HTTP source returned an unsuccessful status",
            Self::DeclaredTooLarge { .. }
            | Self::DownloadTooLarge { .. }
            | Self::DecodedTooLarge { .. } => "source artifact exceeded configured limits",
            Self::ArtifactIo(_) => "temporary artifact I/O failed",
            Self::StagingTooLarge { .. } => "prepared rows exceeded configured limits",
            Self::InvalidArchive | Self::AmbiguousZip => "source archive was invalid",
            Self::Parse { .. } => "source artifact could not be parsed",
            Self::EmptySnapshot { .. } => "source artifact contained no usable records",
            Self::EndpointProtection => "source stream endpoint could not be protected",
            Self::Storage(_) => "source snapshot transaction failed",
            Self::Cancelled => "ingestion job was cancelled",
            Self::OwnershipLost => "ingestion job ownership was lost",
            Self::InvalidRequest(_) => "ingestion request was invalid",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::IngestError;

    #[test]
    fn every_ingest_error_has_a_stable_redacted_summary() {
        let errors = [
            (IngestError::HttpRequest, "HTTP request failed"),
            (
                IngestError::HttpStatus(503),
                "HTTP source returned an unsuccessful status",
            ),
            (
                IngestError::DeclaredTooLarge { limit: 1 },
                "source artifact exceeded configured limits",
            ),
            (
                IngestError::DownloadTooLarge { limit: 2 },
                "source artifact exceeded configured limits",
            ),
            (
                IngestError::DecodedTooLarge { limit: 3 },
                "source artifact exceeded configured limits",
            ),
            (
                IngestError::ArtifactIo(std::io::Error::other("super-secret")),
                "temporary artifact I/O failed",
            ),
            (
                IngestError::StagingTooLarge { limit: 4 },
                "prepared rows exceeded configured limits",
            ),
            (IngestError::InvalidArchive, "source archive was invalid"),
            (IngestError::AmbiguousZip, "source archive was invalid"),
            (
                IngestError::Parse {
                    format: "M3U",
                    source: iptv_parsers::ParseError::MalformedM3u("super-secret".to_owned()),
                },
                "source artifact could not be parsed",
            ),
            (
                IngestError::EmptySnapshot { format: "XMLTV" },
                "source artifact contained no usable records",
            ),
            (
                IngestError::EndpointProtection,
                "source stream endpoint could not be protected",
            ),
            (
                IngestError::Storage(sqlx::Error::RowNotFound),
                "source snapshot transaction failed",
            ),
            (IngestError::Cancelled, "ingestion job was cancelled"),
            (
                IngestError::OwnershipLost,
                "ingestion job ownership was lost",
            ),
            (
                IngestError::InvalidRequest("super-secret"),
                "ingestion request was invalid",
            ),
        ];

        for (error, expected) in errors {
            assert_eq!(error.persisted_summary(), expected);
            assert!(!error.persisted_summary().contains("super-secret"));
        }
    }
}
