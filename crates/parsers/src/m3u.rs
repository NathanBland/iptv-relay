use crate::{Diagnostic, DiagnosticCode, ParseError, ParseLimits, ParseStats};
use std::{collections::BTreeMap, io::BufRead};
use url::Url;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct M3uHeader {
    pub attributes: BTreeMap<String, String>,
    pub raw: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct M3uDirective {
    /// Directive name without the leading `#`, preserving source case.
    pub name: String,
    pub value: Option<String>,
    pub raw: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct M3uEntry {
    pub duration_seconds: Option<f64>,
    pub title: String,
    /// Lower-cased attribute names, including unknown provider-specific attributes.
    pub attributes: BTreeMap<String, String>,
    pub url: Url,
    /// All non-`EXTINF` directives associated with the entry, including unknown ones.
    pub directives: Vec<M3uDirective>,
    pub raw_extinf: Option<String>,
}

impl M3uEntry {
    pub fn tvg_id(&self) -> Option<&str> {
        self.attributes.get("tvg-id").map(String::as_str)
    }

    pub fn group_title(&self) -> Option<&str> {
        self.attributes.get("group-title").map(String::as_str)
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct M3uPlaylist {
    pub header: M3uHeader,
    pub entries: Vec<M3uEntry>,
    pub global_directives: Vec<M3uDirective>,
    pub diagnostics: Vec<Diagnostic>,
    pub stats: ParseStats,
}

#[derive(Debug)]
struct PendingEntry {
    duration_seconds: Option<f64>,
    title: String,
    attributes: BTreeMap<String, String>,
    directives: Vec<M3uDirective>,
    raw_extinf: String,
    line: u64,
}

/// Parses an extended M3U from a bounded, incrementally consumed reader.
///
/// # Errors
///
/// Returns [`ParseError`] for I/O failures, invalid UTF-8, or configured input,
/// line, and record-limit violations. Malformed individual entries are reported
/// through diagnostics where safe recovery is possible.
#[allow(clippy::too_many_lines)]
pub fn parse_m3u<R: BufRead>(reader: R, limits: ParseLimits) -> Result<M3uPlaylist, ParseError> {
    let mut entries = Vec::new();
    let mut playlist = parse_m3u_visit(reader, limits, |entry| {
        entries.push(entry);
        Ok(())
    })?;
    playlist.entries = entries;
    Ok(playlist)
}

/// Parses an M3U while handing each completed entry to a visitor immediately.
///
/// The returned playlist contains header/global metadata and diagnostics but an
/// empty `entries` vector. This keeps entry memory bounded by the caller's batch.
///
/// # Errors
///
/// Returns parser limit/I/O errors or an error returned by the visitor.
#[allow(clippy::too_many_lines)]
pub fn parse_m3u_visit<R, F>(
    reader: R,
    limits: ParseLimits,
    mut visit: F,
) -> Result<M3uPlaylist, ParseError>
where
    R: BufRead,
    F: FnMut(M3uEntry) -> Result<(), ParseError>,
{
    let mut reader = BoundedLineReader::new(reader, limits);
    let mut playlist = M3uPlaylist::default();
    let mut pending: Option<PendingEntry> = None;
    let mut saw_content = false;
    let mut saw_header = false;

    while let Some((line_number, line)) = reader.next_line()? {
        playlist.stats.lines_read += 1;
        let line = line.strip_prefix('\u{feff}').unwrap_or(&line).trim();
        if line.is_empty() {
            continue;
        }

        if !saw_content {
            saw_content = true;
            if line.starts_with("#EXTM3U") {
                saw_header = true;
                playlist.header = parse_header(line, &mut playlist.diagnostics, line_number);
                continue;
            }
            push_diagnostic(
                &mut playlist,
                Diagnostic::warning(
                    DiagnosticCode::MissingHeader,
                    Some(line_number),
                    None,
                    "playlist does not begin with #EXTM3U",
                ),
            );
        }

        if line.starts_with("#EXTINF:") {
            if record_limit_reached(playlist.stats.records_seen, limits.max_records) {
                return Err(ParseError::TooManyRecords {
                    limit: limits.max_records,
                });
            }
            playlist.stats.records_seen += 1;
            if let Some(previous) = pending.take() {
                playlist.stats.records_skipped += 1;
                push_diagnostic(
                    &mut playlist,
                    Diagnostic::warning(
                        DiagnosticCode::ReplacedPendingEntry,
                        Some(previous.line),
                        None,
                        "EXTINF was replaced by another EXTINF before a URL was found",
                    ),
                );
            }
            pending = Some(parse_extinf(
                line,
                line_number,
                &mut playlist.diagnostics,
                &mut playlist.stats,
            ));
            continue;
        }

        if line.starts_with('#') {
            let directive = parse_directive(line);
            if !matches!(directive.name.as_str(), "EXTGRP" | "EXTVLCOPT") {
                playlist.stats.unknown_metadata += 1;
            }
            if let Some(entry) = pending.as_mut() {
                if directive.name.eq_ignore_ascii_case("EXTGRP")
                    && !entry.attributes.contains_key("group-title")
                    && let Some(value) = directive.value.as_deref()
                {
                    entry
                        .attributes
                        .insert("group-title".to_owned(), value.trim().to_owned());
                }
                entry.directives.push(directive);
            } else {
                playlist.global_directives.push(directive);
            }
            continue;
        }

        match Url::parse(line) {
            Ok(url) => {
                if usize::try_from(playlist.stats.records_emitted).unwrap_or(usize::MAX)
                    >= limits.max_records
                {
                    return Err(ParseError::TooManyRecords {
                        limit: limits.max_records,
                    });
                }
                let entry = if let Some(pending) = pending.take() {
                    M3uEntry {
                        duration_seconds: pending.duration_seconds,
                        title: pending.title,
                        attributes: pending.attributes,
                        url,
                        directives: pending.directives,
                        raw_extinf: Some(pending.raw_extinf),
                    }
                } else {
                    push_diagnostic(
                        &mut playlist,
                        Diagnostic::warning(
                            DiagnosticCode::OrphanUrl,
                            Some(line_number),
                            None,
                            "URL has no preceding EXTINF; preserving it as an unnamed entry",
                        ),
                    );
                    playlist.stats.records_seen += 1;
                    M3uEntry {
                        duration_seconds: None,
                        title: String::new(),
                        attributes: BTreeMap::new(),
                        url,
                        directives: Vec::new(),
                        raw_extinf: None,
                    }
                };
                visit(entry)?;
                playlist.stats.records_emitted += 1;
            }
            Err(error) => {
                playlist.stats.records_skipped += u64::from(pending.take().is_some());
                push_diagnostic(
                    &mut playlist,
                    Diagnostic::error(
                        DiagnosticCode::InvalidUrl,
                        Some(line_number),
                        None,
                        format!("invalid absolute stream URL: {error}"),
                    ),
                );
            }
        }
    }

    if let Some(entry) = pending {
        playlist.stats.records_skipped += 1;
        push_diagnostic(
            &mut playlist,
            Diagnostic::warning(
                DiagnosticCode::OrphanMetadata,
                Some(entry.line),
                None,
                "EXTINF at end of input has no URL",
            ),
        );
    }
    if !saw_header && !saw_content {
        push_diagnostic(
            &mut playlist,
            Diagnostic::warning(DiagnosticCode::MissingHeader, None, None, "empty playlist"),
        );
    }
    playlist.stats.bytes_read = reader.bytes_read;
    playlist.stats.warnings = playlist
        .diagnostics
        .iter()
        .filter(|item| item.severity == crate::Severity::Warning)
        .count()
        .try_into()
        .unwrap_or(u64::MAX);
    playlist.stats.errors = playlist
        .diagnostics
        .iter()
        .filter(|item| item.severity == crate::Severity::Error)
        .count()
        .try_into()
        .unwrap_or(u64::MAX);
    Ok(playlist)
}

fn parse_header(line: &str, diagnostics: &mut Vec<Diagnostic>, line_number: u64) -> M3uHeader {
    let tail = line.strip_prefix("#EXTM3U").unwrap_or_default().trim();
    M3uHeader {
        attributes: parse_attributes(tail, diagnostics, line_number),
        raw: line.to_owned(),
    }
}

fn parse_extinf(
    line: &str,
    line_number: u64,
    diagnostics: &mut Vec<Diagnostic>,
    stats: &mut ParseStats,
) -> PendingEntry {
    let body = line.strip_prefix("#EXTINF:").unwrap_or_default();
    let comma = comma_outside_quotes(body);
    let (metadata, title) = comma.map_or((body, ""), |index| (&body[..index], &body[index + 1..]));
    let metadata = metadata.trim();
    let split = metadata.find(char::is_whitespace).unwrap_or(metadata.len());
    let duration_raw = &metadata[..split];
    let attribute_raw = metadata[split..].trim();
    let duration_seconds = if duration_raw.is_empty() {
        None
    } else {
        match duration_raw.parse::<f64>() {
            Ok(value) if value.is_finite() => Some(value),
            _ => {
                let diagnostic = Diagnostic::warning(
                    DiagnosticCode::InvalidDuration,
                    Some(line_number),
                    None,
                    format!("invalid EXTINF duration {duration_raw:?}"),
                );
                stats.observe(&diagnostic);
                diagnostics.push(diagnostic);
                None
            }
        }
    };

    PendingEntry {
        duration_seconds,
        title: title.trim().to_owned(),
        attributes: parse_attributes(attribute_raw, diagnostics, line_number),
        directives: Vec::new(),
        raw_extinf: line.to_owned(),
        line: line_number,
    }
}

fn comma_outside_quotes(value: &str) -> Option<usize> {
    let mut quote = None;
    for (index, character) in value.char_indices() {
        match character {
            '\'' | '"' if quote == Some(character) => quote = None,
            '\'' | '"' if quote.is_none() => quote = Some(character),
            ',' if quote.is_none() => return Some(index),
            _ => {}
        }
    }
    None
}

fn parse_attributes(
    value: &str,
    diagnostics: &mut Vec<Diagnostic>,
    line_number: u64,
) -> BTreeMap<String, String> {
    let bytes = value.as_bytes();
    let mut index = 0;
    let mut attributes = BTreeMap::new();
    while index < bytes.len() {
        while bytes.get(index).is_some_and(u8::is_ascii_whitespace) {
            index += 1;
        }
        if index == bytes.len() {
            break;
        }
        let key_start = index;
        while index < bytes.len() && !bytes[index].is_ascii_whitespace() && bytes[index] != b'=' {
            index += 1;
        }
        let key = &value[key_start..index];
        while bytes.get(index).is_some_and(u8::is_ascii_whitespace) {
            index += 1;
        }
        if bytes.get(index) != Some(&b'=') {
            diagnostics.push(Diagnostic::warning(
                DiagnosticCode::InvalidAttribute,
                Some(line_number),
                None,
                format!("attribute {key:?} has no '=' and was ignored"),
            ));
            while index < bytes.len() && !bytes[index].is_ascii_whitespace() {
                index += 1;
            }
            continue;
        }
        index += 1;
        while bytes.get(index).is_some_and(u8::is_ascii_whitespace) {
            index += 1;
        }
        let attribute_value;
        if matches!(bytes.get(index), Some(b'\'' | b'"')) {
            let quote = bytes[index];
            index += 1;
            let start = index;
            while index < bytes.len() && bytes[index] != quote {
                index += 1;
            }
            attribute_value = value[start..index].to_owned();
            if index == bytes.len() {
                diagnostics.push(Diagnostic::warning(
                    DiagnosticCode::InvalidAttribute,
                    Some(line_number),
                    None,
                    format!("attribute {key:?} has an unterminated quote"),
                ));
            } else {
                index += 1;
            }
        } else {
            let start = index;
            while index < bytes.len() && !bytes[index].is_ascii_whitespace() {
                index += 1;
            }
            attribute_value = value[start..index].to_owned();
        }
        if !key.is_empty() {
            attributes.insert(key.to_ascii_lowercase(), attribute_value);
        }
    }
    attributes
}

fn parse_directive(line: &str) -> M3uDirective {
    let without_hash = line.strip_prefix('#').unwrap_or(line);
    let (name, value) = without_hash
        .split_once(':')
        .map_or((without_hash, None), |(name, value)| {
            (name, Some(value.to_owned()))
        });
    M3uDirective {
        name: name.to_owned(),
        value,
        raw: line.to_owned(),
    }
}

fn push_diagnostic(playlist: &mut M3uPlaylist, diagnostic: Diagnostic) {
    playlist.stats.observe(&diagnostic);
    playlist.diagnostics.push(diagnostic);
}

fn record_limit_reached(seen: u64, maximum: usize) -> bool {
    usize::try_from(seen).map_or(true, |seen| seen >= maximum)
}

struct BoundedLineReader<R> {
    inner: R,
    limits: ParseLimits,
    bytes_read: u64,
    line_number: u64,
}

impl<R: BufRead> BoundedLineReader<R> {
    fn new(inner: R, limits: ParseLimits) -> Self {
        Self {
            inner,
            limits,
            bytes_read: 0,
            line_number: 0,
        }
    }

    fn next_line(&mut self) -> Result<Option<(u64, String)>, ParseError> {
        let mut line = Vec::new();
        loop {
            let available = self.inner.fill_buf()?;
            if available.is_empty() {
                if line.is_empty() {
                    return Ok(None);
                }
                break;
            }
            let take = available
                .iter()
                .position(|byte| *byte == b'\n')
                .map_or(available.len(), |index| index + 1);
            if line.len().saturating_add(take) > self.limits.max_line_bytes {
                return Err(ParseError::LineTooLong {
                    line: self.line_number + 1,
                    limit: self.limits.max_line_bytes,
                });
            }
            let chunk = available[..take].to_vec();
            self.inner.consume(take);
            self.bytes_read = self
                .bytes_read
                .saturating_add(u64::try_from(take).unwrap_or(u64::MAX));
            if self.bytes_read > self.limits.max_input_bytes {
                return Err(ParseError::InputTooLarge {
                    limit: self.limits.max_input_bytes,
                });
            }
            let found_newline = chunk.last() == Some(&b'\n');
            line.extend_from_slice(&chunk);
            if found_newline {
                break;
            }
        }
        self.line_number += 1;
        while matches!(line.last(), Some(b'\n' | b'\r')) {
            line.pop();
        }
        let line = String::from_utf8(line).map_err(|_| ParseError::InvalidUtf8 {
            line: self.line_number,
        })?;
        Ok(Some((self.line_number, line)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use std::io::Cursor;

    const FIXTURE: &str = include_str!("../../../tests/fixtures/m3u/extended.m3u");

    #[test]
    fn parses_extended_playlist_and_preserves_unknown_metadata() {
        let playlist = parse_m3u(Cursor::new(FIXTURE), ParseLimits::default()).expect("parse");
        assert_eq!(playlist.entries.len(), 2);
        let first = &playlist.entries[0];
        assert_eq!(first.tvg_id(), Some("kusa.denver.example"));
        assert_eq!(first.group_title(), Some("Denver Locals"));
        assert_eq!(first.title, "KUSA, Denver");
        assert_eq!(
            first.attributes.get("provider-custom"),
            Some(&"alpha".to_owned())
        );
        assert!(first.directives.iter().any(|item| item.name == "KODIPROP"));
        assert!(first.directives.iter().any(|item| item.name == "EXTVLCOPT"));
        assert_eq!(
            playlist.header.attributes.get("x-tvg-url"),
            Some(&"https://guide.example/guide.xml".to_owned())
        );
        assert!(playlist.stats.unknown_metadata >= 1);
    }

    #[test]
    fn extgrp_is_used_only_when_group_title_is_absent() {
        let input = "#EXTM3U\n#EXTINF:-1 group-title=\"explicit\",One\n#EXTGRP:fallback\nhttp://example/one\n#EXTINF:-1,Two\n#EXTGRP:fallback\nhttp://example/two\n";
        let playlist = parse_m3u(Cursor::new(input), ParseLimits::default()).expect("parse");
        assert_eq!(playlist.entries[0].group_title(), Some("explicit"));
        assert_eq!(playlist.entries[1].group_title(), Some("fallback"));
    }

    #[test]
    fn rejects_oversized_lines_before_unbounded_growth() {
        let input = format!("#EXTM3U\n{}\n", "x".repeat(33));
        let limits = ParseLimits {
            max_line_bytes: 32,
            ..ParseLimits::default()
        };
        assert!(matches!(
            parse_m3u(Cursor::new(input), limits),
            Err(ParseError::LineTooLong { line: 2, limit: 32 })
        ));
    }

    #[test]
    fn reports_orphan_metadata_and_keeps_absolute_bare_urls() {
        let input = "#EXTM3U\n#EXTINF:-1,Lost\n#EXTINF:-1,Kept\nhttps://example.test/live.ts\nhttps://example.test/bare.ts\n";
        let playlist = parse_m3u(Cursor::new(input), ParseLimits::default()).expect("parse");
        assert_eq!(playlist.entries.len(), 2);
        assert!(
            playlist
                .diagnostics
                .iter()
                .any(|item| item.code == DiagnosticCode::ReplacedPendingEntry)
        );
        assert!(
            playlist
                .diagnostics
                .iter()
                .any(|item| item.code == DiagnosticCode::OrphanUrl)
        );
    }

    #[test]
    fn enforces_input_and_record_limits() {
        let input = "#EXTM3U\n#EXTINF:-1,One\nhttps://example.test/one\n#EXTINF:-1,Two\nhttps://example.test/two\n";
        let records = ParseLimits {
            max_records: 1,
            ..ParseLimits::default()
        };
        assert!(matches!(
            parse_m3u(Cursor::new(input), records),
            Err(ParseError::TooManyRecords { limit: 1 })
        ));

        let bytes = ParseLimits {
            max_input_bytes: 10,
            ..ParseLimits::default()
        };
        assert!(matches!(
            parse_m3u(Cursor::new(input), bytes),
            Err(ParseError::InputTooLarge { limit: 10 })
        ));
    }

    #[test]
    fn invalid_utf8_is_a_fatal_located_error() {
        let input = [b'#', b'E', b'X', b'T', b'M', b'3', b'U', b'\n', 0xff, b'\n'];
        assert!(matches!(
            parse_m3u(Cursor::new(input), ParseLimits::default()),
            Err(ParseError::InvalidUtf8 { line: 2 })
        ));
    }

    #[test]
    fn recovers_missing_header_empty_input_and_trailing_pending_entries() {
        let missing = parse_m3u(
            Cursor::new("\u{feff}\n#EXTINF:NaN broken custom=\"unterminated\"\n"),
            ParseLimits::default(),
        )
        .expect("recover malformed entry");
        assert!(missing.entries.is_empty());
        assert!(
            missing
                .diagnostics
                .iter()
                .any(|item| { item.code == DiagnosticCode::MissingHeader && item.line == Some(2) })
        );
        assert!(
            missing
                .diagnostics
                .iter()
                .any(|item| item.code == DiagnosticCode::InvalidDuration)
        );
        assert!(
            missing
                .diagnostics
                .iter()
                .any(|item| item.code == DiagnosticCode::InvalidAttribute)
        );
        assert!(
            missing
                .diagnostics
                .iter()
                .any(|item| item.code == DiagnosticCode::OrphanMetadata)
        );

        let empty = parse_m3u(Cursor::new("\n \r\n"), ParseLimits::default()).expect("empty");
        assert!(empty.entries.is_empty());
        assert_eq!(empty.diagnostics[0].code, DiagnosticCode::MissingHeader);
    }

    #[test]
    fn handles_global_directives_quoted_commas_invalid_urls_and_bare_lines() {
        let input = "#EXTM3U\n#EXT-X-VERSION:3\n#EXTINF:1.5 custom='a,b' bare,Title, with comma\nhttps://example.test/final.ts\nnot-a-url";
        let playlist = parse_m3u(Cursor::new(input), ParseLimits::default()).expect("recover");
        assert_eq!(playlist.global_directives[0].value.as_deref(), Some("3"));
        assert_eq!(playlist.entries.len(), 1);
        assert_eq!(playlist.entries[0].title, "Title, with comma");
        assert_eq!(playlist.entries[0].duration_seconds, Some(1.5));
        assert!(
            playlist
                .diagnostics
                .iter()
                .any(|item| item.code == DiagnosticCode::InvalidUrl)
        );
        assert!(
            playlist
                .diagnostics
                .iter()
                .any(|item| item.code == DiagnosticCode::InvalidAttribute)
        );
    }

    #[test]
    fn visitor_errors_are_returned_and_final_unterminated_line_is_counted() {
        let error = parse_m3u_visit(
            Cursor::new("#EXTM3U\n#EXTINF:-1,One\nhttps://example.test/one.ts"),
            ParseLimits::default(),
            |_| Err(ParseError::MalformedM3u("visitor stopped".to_owned())),
        )
        .expect_err("visitor error");
        assert!(matches!(error, ParseError::MalformedM3u(message) if message == "visitor stopped"));

        let playlist = parse_m3u(
            Cursor::new("#EXTM3U\nhttps://example.test/no-newline.ts"),
            ParseLimits::default(),
        )
        .expect("final line");
        assert_eq!(playlist.entries.len(), 1);
        assert_eq!(playlist.stats.lines_read, 2);
    }

    #[test]
    fn visitor_output_is_equivalent_and_not_retained_by_summary() {
        let collected = parse_m3u(Cursor::new(FIXTURE), ParseLimits::default()).expect("collect");
        let mut visited = Vec::new();
        let summary = parse_m3u_visit(Cursor::new(FIXTURE), ParseLimits::default(), |entry| {
            visited.push(entry);
            Ok(())
        })
        .expect("visit");
        assert!(summary.entries.is_empty());
        assert_eq!(visited, collected.entries);
        assert_eq!(summary.header, collected.header);
        assert_eq!(summary.global_directives, collected.global_directives);
        assert_eq!(summary.diagnostics, collected.diagnostics);
        assert_eq!(summary.stats, collected.stats);
    }

    proptest! {
        #[test]
        fn quoted_attribute_values_round_trip(value in "[A-Za-z0-9 _.,-]{0,80}") {
            let escaped = value.replace('"', "");
            let input = format!("#EXTM3U\n#EXTINF:-1 custom=\"{escaped}\",Title\nhttps://example.test/live.ts\n");
            let playlist = parse_m3u(Cursor::new(input), ParseLimits::default()).expect("parse");
            prop_assert_eq!(playlist.entries[0].attributes.get("custom"), Some(&escaped));
        }
    }
}
