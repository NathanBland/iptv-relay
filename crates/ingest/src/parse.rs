use crate::{DecodedArtifact, IngestError, IngestFormat, XtreamPayloadKind};
use iptv_parsers::{
    M3uPlaylist, ParseLimits, ParseStats, XmltvDocument, XtreamAuth, XtreamCategory,
    XtreamDocument, XtreamLiveStream, XtreamShortEpgEntry, parse_m3u, parse_xmltv_with_options,
    parse_xtream_auth, parse_xtream_live_categories, parse_xtream_live_streams,
    parse_xtream_short_epg_in_timezone,
};
use std::io::BufReader;

#[derive(Debug)]
pub enum ParsedArtifact {
    M3u(M3uPlaylist),
    Xmltv(XmltvDocument),
    XtreamAuth(XtreamDocument<XtreamAuth>),
    XtreamCategories(XtreamDocument<XtreamCategory>),
    XtreamStreams(XtreamDocument<XtreamLiveStream>),
    XtreamEpg(XtreamDocument<XtreamShortEpgEntry>),
}

impl ParsedArtifact {
    pub const fn format_name(&self) -> &'static str {
        match self {
            Self::M3u(_) => "M3U",
            Self::Xmltv(_) => "XMLTV",
            Self::XtreamAuth(_) => "Xtream auth",
            Self::XtreamCategories(_) => "Xtream live categories",
            Self::XtreamStreams(_) => "Xtream live streams",
            Self::XtreamEpg(_) => "Xtream short EPG",
        }
    }

    pub fn stats(&self) -> &ParseStats {
        match self {
            Self::M3u(document) => &document.stats,
            Self::Xmltv(document) => &document.stats,
            Self::XtreamAuth(document) => &document.stats,
            Self::XtreamCategories(document) => &document.stats,
            Self::XtreamStreams(document) => &document.stats,
            Self::XtreamEpg(document) => &document.stats,
        }
    }

    pub fn usable_record_count(&self) -> usize {
        match self {
            Self::M3u(document) => document.entries.len(),
            Self::Xmltv(document) => document.channels.len() + document.programmes.len(),
            Self::XtreamAuth(document) => document
                .records
                .iter()
                .filter(|record| record.authenticated)
                .count(),
            Self::XtreamCategories(document) => document.records.len(),
            Self::XtreamStreams(document) => document.records.len(),
            Self::XtreamEpg(document) => document.records.len(),
        }
    }
}

#[cfg(test)]
#[allow(clippy::missing_errors_doc)]
pub fn parse_artifact(
    artifact: &DecodedArtifact,
    format: IngestFormat,
    limits: ParseLimits,
) -> Result<ParsedArtifact, IngestError> {
    parse_artifact_with_source_timezone(artifact, format, limits, "UTC")
}

/// Parses one decoded artifact with the configured source timezone.
///
/// The timezone applies to timestamps without an explicit offset. Explicit
/// timestamp offsets remain authoritative.
#[allow(clippy::missing_errors_doc)]
pub fn parse_artifact_with_source_timezone(
    artifact: &DecodedArtifact,
    format: IngestFormat,
    mut limits: ParseLimits,
    source_timezone: &str,
) -> Result<ParsedArtifact, IngestError> {
    limits.max_input_bytes = limits
        .max_input_bytes
        .min(artifact.decoded_byte_count.max(1));
    let file = artifact.try_clone_file()?;
    let parsed = match format {
        IngestFormat::M3u => parse_m3u(BufReader::new(file), limits)
            .map(ParsedArtifact::M3u)
            .map_err(|source| IngestError::Parse {
                format: "M3U",
                source,
            })?,
        IngestFormat::Xmltv => parse_xmltv_with_options(
            BufReader::new(file),
            limits,
            &iptv_parsers::XmltvParseOptions {
                default_timezone: source_timezone.to_owned(),
            },
        )
        .map(ParsedArtifact::Xmltv)
        .map_err(|source| IngestError::Parse {
            format: "XMLTV",
            source,
        })?,
        IngestFormat::Xtream(XtreamPayloadKind::Auth) => {
            parse_xtream_auth(BufReader::new(file), limits)
                .map(ParsedArtifact::XtreamAuth)
                .map_err(|source| IngestError::Parse {
                    format: "Xtream auth",
                    source,
                })?
        }
        IngestFormat::Xtream(XtreamPayloadKind::LiveCategories) => {
            parse_xtream_live_categories(BufReader::new(file), limits)
                .map(ParsedArtifact::XtreamCategories)
                .map_err(|source| IngestError::Parse {
                    format: "Xtream live categories",
                    source,
                })?
        }
        IngestFormat::Xtream(XtreamPayloadKind::LiveStreams) => {
            parse_xtream_live_streams(BufReader::new(file), limits)
                .map(ParsedArtifact::XtreamStreams)
                .map_err(|source| IngestError::Parse {
                    format: "Xtream live streams",
                    source,
                })?
        }
        IngestFormat::Xtream(XtreamPayloadKind::ShortEpg) => {
            parse_xtream_short_epg_in_timezone(BufReader::new(file), limits, source_timezone)
                .map(ParsedArtifact::XtreamEpg)
                .map_err(|source| IngestError::Parse {
                    format: "Xtream short EPG",
                    source,
                })?
        }
    };
    if parsed.usable_record_count() == 0 {
        return Err(IngestError::EmptySnapshot {
            format: parsed.format_name(),
        });
    }
    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ArtifactLimits, DownloadedArtifact, unpack_artifact};
    use std::io::Write;

    fn decoded(bytes: &[u8]) -> DecodedArtifact {
        let mut file = tempfile::tempfile().expect("tempfile");
        file.write_all(bytes).expect("write");
        unpack_artifact(
            DownloadedArtifact::from_file(file, None).expect("artifact"),
            ArtifactLimits::default(),
        )
        .expect("decode")
    }

    #[test]
    fn parses_m3u_and_rejects_empty_playlist() {
        let parsed = parse_artifact(
            &decoded(b"#EXTM3U\n#EXTINF:-1 tvg-id=\"one\",One\nhttps://example.test/one\n"),
            IngestFormat::M3u,
            ParseLimits::default(),
        )
        .expect("M3U");
        assert_eq!(parsed.usable_record_count(), 1);
        assert_eq!(parsed.stats().records_emitted, 1);
        assert!(matches!(
            parse_artifact(
                &decoded(b"#EXTM3U\n"),
                IngestFormat::M3u,
                ParseLimits::default()
            ),
            Err(IngestError::EmptySnapshot { format: "M3U" })
        ));
    }

    #[test]
    fn parses_xmltv_and_rejects_empty_document() {
        let parsed = parse_artifact(
            &decoded(b"<tv><channel id=\"one\"><display-name>One</display-name></channel></tv>"),
            IngestFormat::Xmltv,
            ParseLimits::default(),
        )
        .expect("XMLTV");
        assert_eq!(parsed.usable_record_count(), 1);
        assert!(matches!(
            parse_artifact(
                &decoded(b"<tv/>"),
                IngestFormat::Xmltv,
                ParseLimits::default()
            ),
            Err(IngestError::EmptySnapshot { format: "XMLTV" })
        ));
    }

    #[test]
    fn xmltv_source_timezone_is_fallback_for_implicit_timestamps() {
        let parsed = parse_artifact_with_source_timezone(
            &decoded(
                br#"<tv>
                    <channel id="denver"/>
                    <programme channel="denver" start="20260101120000" stop="20260101130000"><title>Winter local noon</title></programme>
                    <programme channel="denver" start="20260101120000 +0000" stop="20260101130000 +0000"><title>Explicit UTC noon</title></programme>
                </tv>"#,
            ),
            IngestFormat::Xmltv,
            ParseLimits::default(),
            "America/Denver",
        )
        .expect("XMLTV with source timezone");

        let ParsedArtifact::Xmltv(document) = parsed else {
            panic!("expected XMLTV document");
        };
        assert_eq!(document.programmes.len(), 2);
        assert_eq!(
            document.programmes[0].start.to_rfc3339(),
            "2026-01-01T19:00:00+00:00"
        );
        assert_eq!(
            document.programmes[1].start.to_rfc3339(),
            "2026-01-01T12:00:00+00:00"
        );
    }

    #[test]
    fn parses_every_supported_xtream_payload_shape() {
        let cases = [
            (
                XtreamPayloadKind::Auth,
                br#"{"user_info":{"auth":1},"server_info":{}}"#.as_slice(),
            ),
            (
                XtreamPayloadKind::LiveCategories,
                br#"[{"category_id":"1","category_name":"News"}]"#.as_slice(),
            ),
            (
                XtreamPayloadKind::LiveStreams,
                br#"[{"stream_id":7,"name":"News"}]"#.as_slice(),
            ),
            (
                XtreamPayloadKind::ShortEpg,
                br#"{"epg_listings":[{"title":"TmV3cw==","description":"","start_timestamp":1700000000,"stop_timestamp":1700003600}]}"#.as_slice(),
            ),
        ];
        let names = [
            "Xtream auth",
            "Xtream live categories",
            "Xtream live streams",
            "Xtream short EPG",
        ];
        for ((kind, input), name) in cases.into_iter().zip(names) {
            let parsed = parse_artifact(
                &decoded(input),
                IngestFormat::Xtream(kind),
                ParseLimits::default(),
            )
            .expect("Xtream payload");
            assert_eq!(parsed.usable_record_count(), 1);
            assert_eq!(parsed.format_name(), name);
            assert_eq!(parsed.stats().records_seen, 1);
        }
    }

    #[test]
    fn malformed_inputs_report_the_dispatch_format() {
        let cases = [
            (IngestFormat::M3u, b"#EXTM3U\n\xff\n".as_slice(), "M3U"),
            (IngestFormat::Xmltv, b"<tv".as_slice(), "XMLTV"),
            (
                IngestFormat::Xtream(XtreamPayloadKind::Auth),
                br"{}".as_slice(),
                "Xtream auth",
            ),
            (
                IngestFormat::Xtream(XtreamPayloadKind::LiveCategories),
                br"{}".as_slice(),
                "Xtream live categories",
            ),
            (
                IngestFormat::Xtream(XtreamPayloadKind::LiveStreams),
                br"{}".as_slice(),
                "Xtream live streams",
            ),
            (
                IngestFormat::Xtream(XtreamPayloadKind::ShortEpg),
                br"{}".as_slice(),
                "Xtream short EPG",
            ),
        ];
        for (format, input, expected) in cases {
            let error = parse_artifact(&decoded(input), format, ParseLimits::default())
                .expect_err("malformed input");
            assert!(matches!(error, IngestError::Parse { format, .. } if format == expected));
        }
    }

    #[test]
    fn unauthenticated_xtream_auth_is_not_a_usable_snapshot() {
        assert!(matches!(
            parse_artifact(
                &decoded(br#"{"user_info":{"auth":0}}"#),
                IngestFormat::Xtream(XtreamPayloadKind::Auth),
                ParseLimits::default()
            ),
            Err(IngestError::EmptySnapshot {
                format: "Xtream auth"
            })
        ));
    }

    #[test]
    fn malformed_input_reports_only_format_in_display() {
        let error = parse_artifact(
            &decoded(b"{password=https://alice:secret@example.test}"),
            IngestFormat::Xtream(XtreamPayloadKind::LiveStreams),
            ParseLimits::default(),
        )
        .expect_err("malformed");
        assert_eq!(
            error.to_string(),
            "artifact parse failed for Xtream live streams"
        );
        assert!(!error.to_string().contains("secret"));
    }
}
