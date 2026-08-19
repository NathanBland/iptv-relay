use crate::{Diagnostic, DiagnosticCode, ParseError, ParseLimits, ParseStats};
use chrono::{DateTime, FixedOffset, LocalResult, NaiveDate, Offset, TimeZone, Utc};
use chrono_tz::Tz;
use iptv_domain::{
    Credit, Credits, EpgChannel, EpgIcon, EpisodeNumber, ExtensionElement, LocalizedText,
    Programme, ProgrammeRating,
};
use quick_xml::{
    Reader, XmlVersion,
    encoding::Decoder,
    events::{BytesStart, Event},
};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    io::BufRead,
    str::FromStr,
};
use thiserror::Error;
use url::Url;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TimestampPrecision {
    Year,
    Month,
    Day,
    Hour,
    Minute,
    Second,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct XmltvTimestamp {
    pub utc: DateTime<Utc>,
    pub precision: TimestampPrecision,
    pub offset_seconds: i32,
    pub timezone_was_implicit: bool,
    pub original: String,
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum XmltvTimestampError {
    #[error("XMLTV timestamp is empty")]
    Empty,
    #[error("invalid XMLTV timestamp digits {0:?}")]
    InvalidDigits(String),
    #[error("unsupported XMLTV timestamp precision of {0} digits")]
    UnsupportedPrecision(usize),
    #[error("invalid XMLTV timezone {0:?}")]
    InvalidTimezone(String),
    #[error("local XMLTV time {value:?} is ambiguous in timezone {timezone}")]
    Ambiguous { value: String, timezone: String },
    #[error("local XMLTV time {value:?} does not exist in timezone {timezone}")]
    Nonexistent { value: String, timezone: String },
}

#[derive(Clone, Debug, Default)]
pub struct XmltvDocument {
    pub generator: BTreeMap<String, String>,
    pub channels: Vec<EpgChannel>,
    pub programmes: Vec<Programme>,
    pub diagnostics: Vec<Diagnostic>,
    pub stats: ParseStats,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct XmltvParseOptions {
    pub default_timezone: String,
}

impl Default for XmltvParseOptions {
    fn default() -> Self {
        Self {
            default_timezone: "UTC".to_owned(),
        }
    }
}

#[derive(Debug, Default)]
struct ChannelBuilder {
    id: Option<String>,
    display_names: Vec<LocalizedText>,
    icons: Vec<EpgIcon>,
    urls: Vec<Url>,
    extensions: Vec<ExtensionElement>,
}

#[derive(Debug, Default)]
struct ProgrammeBuilder {
    channel_id: Option<String>,
    start: Option<String>,
    stop: Option<String>,
    titles: Vec<LocalizedText>,
    sub_titles: Vec<LocalizedText>,
    descriptions: Vec<LocalizedText>,
    categories: Vec<LocalizedText>,
    episode_numbers: Vec<EpisodeNumber>,
    icons: Vec<EpgIcon>,
    ratings: Vec<ProgrammeRating>,
    credits: Credits,
    previously_shown: bool,
    new: bool,
    extensions: Vec<ExtensionElement>,
}

#[derive(Debug)]
struct Capture {
    name: String,
    attributes: BTreeMap<String, String>,
    text: String,
    depth: usize,
    child: Option<ExtensionElement>,
    children: Vec<ExtensionElement>,
}

/// Pull-parses an XMLTV document while enforcing byte, text, depth, and record limits.
///
/// # Errors
///
/// Returns [`ParseError`] for malformed XML, forbidden document types, I/O
/// failures, or configured resource-limit violations. Invalid individual channel
/// and programme records are skipped with diagnostics when recovery is safe.
#[allow(clippy::too_many_lines)]
pub fn parse_xmltv<R: BufRead>(
    reader: R,
    limits: ParseLimits,
) -> Result<XmltvDocument, ParseError> {
    parse_xmltv_with_options(reader, limits, &XmltvParseOptions::default())
}

#[allow(clippy::missing_errors_doc)]
pub fn parse_xmltv_with_options<R: BufRead>(
    reader: R,
    limits: ParseLimits,
    options: &XmltvParseOptions,
) -> Result<XmltvDocument, ParseError> {
    let mut channels: Vec<EpgChannel> = Vec::new();
    let mut channel_indexes = HashMap::new();
    let mut programmes = Vec::new();
    let mut document = parse_xmltv_visit_with_options(
        reader,
        limits,
        options,
        |channel| {
            if let Some(index) = channel_indexes.get(&channel.id).copied() {
                merge_channel(&mut channels[index], channel);
            } else {
                channel_indexes.insert(channel.id.clone(), channels.len());
                channels.push(channel);
            }
            Ok(())
        },
        |programme| {
            programmes.push(programme);
            Ok(())
        },
    )?;
    document.channels = channels;
    document.programmes = programmes;
    Ok(document)
}

/// Pull-parses XMLTV and visits each completed channel/programme immediately.
///
/// The returned document retains generator metadata, diagnostics, and stats but
/// leaves `channels` and `programmes` empty, bounding record memory by the
/// caller's batch sizes.
///
/// # Errors
///
/// Returns parser limit/malformed-input errors or an error returned by either visitor.
#[allow(clippy::too_many_lines)]
pub fn parse_xmltv_visit<R, FC, FP>(
    reader: R,
    limits: ParseLimits,
    visit_channel: FC,
    visit_programme: FP,
) -> Result<XmltvDocument, ParseError>
where
    R: BufRead,
    FC: FnMut(EpgChannel) -> Result<(), ParseError>,
    FP: FnMut(Programme) -> Result<(), ParseError>,
{
    parse_xmltv_visit_with_options(
        reader,
        limits,
        &XmltvParseOptions::default(),
        visit_channel,
        visit_programme,
    )
}

#[allow(clippy::missing_errors_doc, clippy::too_many_lines)]
pub fn parse_xmltv_visit_with_options<R, FC, FP>(
    reader: R,
    limits: ParseLimits,
    options: &XmltvParseOptions,
    mut visit_channel: FC,
    mut visit_programme: FP,
) -> Result<XmltvDocument, ParseError>
where
    R: BufRead,
    FC: FnMut(EpgChannel) -> Result<(), ParseError>,
    FP: FnMut(Programme) -> Result<(), ParseError>,
{
    let limited = reader.take(limits.max_input_bytes + 1);
    let mut reader = Reader::from_reader(limited);
    reader.config_mut().trim_text(true);
    let mut buffer = Vec::with_capacity(16 * 1024);
    let mut document = XmltvDocument::default();
    let mut channel = None;
    let mut programme = None;
    let mut capture: Option<Capture> = None;
    let mut depth = 0_usize;
    let mut declared_channels = HashSet::new();
    let mut referenced_channels = HashSet::new();
    let mut programme_fingerprints = HashSet::new();

    loop {
        buffer.clear();
        let event =
            reader
                .read_event_into(&mut buffer)
                .map_err(|error| ParseError::MalformedXmltv {
                    offset: reader.buffer_position(),
                    message: error.to_string(),
                })?;
        let offset = reader.buffer_position();
        if offset > limits.max_input_bytes {
            return Err(ParseError::InputTooLarge {
                limit: limits.max_input_bytes,
            });
        }

        match event {
            Event::Start(element) => {
                depth += 1;
                if depth > limits.max_xml_depth {
                    return Err(ParseError::XmlTooDeep {
                        limit: limits.max_xml_depth,
                    });
                }
                let name = element_name(&element);
                let attributes = element_attributes(&element, reader.decoder(), offset)?;
                if let Some(active) = capture.as_mut() {
                    if active.depth == 1 {
                        active.child = Some(ExtensionElement {
                            name,
                            attributes,
                            text: String::new(),
                        });
                    }
                    active.depth += 1;
                    continue;
                }
                match name.as_str() {
                    "tv" if depth == 1 => document.generator = attributes,
                    "channel" if channel.is_none() && programme.is_none() => {
                        if record_limit_reached(document.stats.records_seen, limits.max_records) {
                            return Err(ParseError::TooManyRecords {
                                limit: limits.max_records,
                            });
                        }
                        document.stats.records_seen += 1;
                        channel = Some(ChannelBuilder {
                            id: attributes.get("id").cloned(),
                            ..ChannelBuilder::default()
                        });
                    }
                    "programme" if channel.is_none() && programme.is_none() => {
                        if record_limit_reached(document.stats.records_seen, limits.max_records) {
                            return Err(ParseError::TooManyRecords {
                                limit: limits.max_records,
                            });
                        }
                        document.stats.records_seen += 1;
                        programme = Some(ProgrammeBuilder {
                            channel_id: attributes.get("channel").cloned(),
                            start: attributes.get("start").cloned(),
                            stop: attributes.get("stop").cloned(),
                            ..ProgrammeBuilder::default()
                        });
                    }
                    _ if channel.is_some() || programme.is_some() => {
                        capture = Some(Capture {
                            name,
                            attributes,
                            text: String::new(),
                            depth: 1,
                            child: None,
                            children: Vec::new(),
                        });
                    }
                    _ => {}
                }
            }
            Event::Empty(element) => {
                let name = element_name(&element);
                let attributes = element_attributes(&element, reader.decoder(), offset)?;
                if let Some(active) = capture.as_mut() {
                    active.children.push(ExtensionElement {
                        name,
                        attributes,
                        text: String::new(),
                    });
                } else if name == "channel" && channel.is_none() && programme.is_none() {
                    if record_limit_reached(document.stats.records_seen, limits.max_records) {
                        return Err(ParseError::TooManyRecords {
                            limit: limits.max_records,
                        });
                    }
                    document.stats.records_seen += 1;
                    if let Some(channel) = finish_channel(
                        ChannelBuilder {
                            id: attributes.get("id").cloned(),
                            ..ChannelBuilder::default()
                        },
                        &mut document,
                        &mut declared_channels,
                        offset,
                    ) {
                        visit_channel(channel)?;
                    }
                } else if name == "programme" && channel.is_none() && programme.is_none() {
                    if record_limit_reached(document.stats.records_seen, limits.max_records) {
                        return Err(ParseError::TooManyRecords {
                            limit: limits.max_records,
                        });
                    }
                    document.stats.records_seen += 1;
                    if let Some(programme) = finish_programme(
                        ProgrammeBuilder {
                            channel_id: attributes.get("channel").cloned(),
                            start: attributes.get("start").cloned(),
                            stop: attributes.get("stop").cloned(),
                            ..ProgrammeBuilder::default()
                        },
                        &mut document,
                        offset,
                        &options.default_timezone,
                    ) {
                        referenced_channels.insert(programme.channel_id.clone());
                        visit_unique_programme(
                            programme,
                            &mut document,
                            &mut programme_fingerprints,
                            offset,
                            &mut visit_programme,
                        )?;
                    }
                } else {
                    handle_empty(
                        &name,
                        attributes,
                        channel.as_mut(),
                        programme.as_mut(),
                        &mut document,
                        offset,
                    );
                }
            }
            Event::Text(text) => {
                if let Some(active) = capture.as_mut() {
                    let decoded = text.decode().map_err(|error| ParseError::MalformedXmltv {
                        offset,
                        message: error.to_string(),
                    })?;
                    let value = quick_xml::escape::unescape(&decoded).map_err(|error| {
                        ParseError::MalformedXmltv {
                            offset,
                            message: error.to_string(),
                        }
                    })?;
                    append_capture_text(active, &value, limits.max_text_bytes)?;
                }
            }
            Event::CData(text) => {
                if let Some(active) = capture.as_mut() {
                    let value = text.decode().map_err(|error| ParseError::MalformedXmltv {
                        offset,
                        message: error.to_string(),
                    })?;
                    append_capture_text(active, &value, limits.max_text_bytes)?;
                }
            }
            Event::End(element) => {
                if let Some(active) = capture.as_mut() {
                    if active.depth == 2
                        && let Some(child) = active.child.take()
                    {
                        active.children.push(child);
                    }
                    active.depth -= 1;
                    if active.depth == 0
                        && let Some(completed) = capture.take()
                    {
                        finish_capture(
                            completed,
                            channel.as_mut(),
                            programme.as_mut(),
                            &mut document,
                            offset,
                        );
                    }
                    depth = depth.saturating_sub(1);
                    continue;
                }

                let name = String::from_utf8_lossy(element.name().as_ref()).into_owned();
                match name.as_str() {
                    "channel" => {
                        if let Some(builder) = channel.take()
                            && let Some(channel) = finish_channel(
                                builder,
                                &mut document,
                                &mut declared_channels,
                                offset,
                            )
                        {
                            visit_channel(channel)?;
                        }
                    }
                    "programme" => {
                        if let Some(builder) = programme.take()
                            && let Some(programme) = finish_programme(
                                builder,
                                &mut document,
                                offset,
                                &options.default_timezone,
                            )
                        {
                            referenced_channels.insert(programme.channel_id.clone());
                            visit_unique_programme(
                                programme,
                                &mut document,
                                &mut programme_fingerprints,
                                offset,
                                &mut visit_programme,
                            )?;
                        }
                    }
                    _ => {}
                }
                depth = depth.saturating_sub(1);
            }
            Event::DocType(doctype) => {
                if doctype.len() > limits.max_text_bytes {
                    return Err(ParseError::TextTooLarge {
                        limit: limits.max_text_bytes,
                    });
                }
                if !is_safe_xmltv_doctype(doctype.as_ref()) {
                    return Err(ParseError::XmlDoctypeForbidden);
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }

    document.stats.bytes_read = reader.buffer_position();
    let undeclared = referenced_channels
        .difference(&declared_channels)
        .cloned()
        .collect::<Vec<_>>();
    for channel_id in undeclared {
        push_diagnostic(
            &mut document,
            Diagnostic::warning(
                DiagnosticCode::UndeclaredChannel,
                None,
                None,
                format!("programme references undeclared channel {channel_id:?}"),
            ),
        );
    }
    Ok(document)
}

fn is_safe_xmltv_doctype(value: &[u8]) -> bool {
    let Ok(value) = std::str::from_utf8(value) else {
        return false;
    };
    let value = value.trim();
    if value.contains(['[', ']', '<', '>']) {
        return false;
    }
    let Some(rest) = value.strip_prefix("tv") else {
        return false;
    };
    if rest.is_empty() {
        return true;
    }
    if !rest.starts_with(char::is_whitespace) {
        return false;
    }
    let external_id = rest.trim_start();
    external_id.starts_with("SYSTEM ") || external_id.starts_with("PUBLIC ")
}

fn element_name(element: &BytesStart<'_>) -> String {
    String::from_utf8_lossy(element.name().as_ref()).into_owned()
}

fn record_limit_reached(seen: u64, maximum: usize) -> bool {
    usize::try_from(seen).map_or(true, |seen| seen >= maximum)
}

fn element_attributes(
    element: &BytesStart<'_>,
    decoder: Decoder,
    offset: u64,
) -> Result<BTreeMap<String, String>, ParseError> {
    let mut attributes = BTreeMap::new();
    for attribute in element.attributes().with_checks(false) {
        let attribute = attribute.map_err(|error| ParseError::MalformedXmltv {
            offset,
            message: error.to_string(),
        })?;
        let key = String::from_utf8_lossy(attribute.key.as_ref()).into_owned();
        let value = attribute
            .decoded_and_normalized_value(XmlVersion::Implicit1_0, decoder)
            .map_err(|error| ParseError::MalformedXmltv {
                offset,
                message: error.to_string(),
            })?
            .into_owned();
        attributes.insert(key, value);
    }
    Ok(attributes)
}

fn append_capture_text(capture: &mut Capture, value: &str, limit: usize) -> Result<(), ParseError> {
    let target = capture
        .child
        .as_mut()
        .map_or(&mut capture.text, |child| &mut child.text);
    if target.len().saturating_add(value.len()) > limit {
        return Err(ParseError::TextTooLarge { limit });
    }
    if !target.is_empty() && !value.is_empty() {
        target.push(' ');
    }
    target.push_str(value);
    Ok(())
}

fn handle_empty(
    name: &str,
    attributes: BTreeMap<String, String>,
    channel: Option<&mut ChannelBuilder>,
    programme: Option<&mut ProgrammeBuilder>,
    document: &mut XmltvDocument,
    offset: u64,
) {
    if name == "icon" {
        if let Some(icon) = parse_icon(&attributes, document, offset) {
            if let Some(channel) = channel {
                channel.icons.push(icon);
            } else if let Some(programme) = programme {
                programme.icons.push(icon);
            }
        }
    } else if let Some(programme) = programme {
        match name {
            "new" => programme.new = true,
            "previously-shown" => programme.previously_shown = true,
            _ => {
                document.stats.unknown_metadata += 1;
                programme.extensions.push(ExtensionElement {
                    name: name.to_owned(),
                    attributes,
                    text: String::new(),
                });
            }
        }
    } else if let Some(channel) = channel {
        document.stats.unknown_metadata += 1;
        channel.extensions.push(ExtensionElement {
            name: name.to_owned(),
            attributes,
            text: String::new(),
        });
    }
}

fn finish_capture(
    capture: Capture,
    channel: Option<&mut ChannelBuilder>,
    programme: Option<&mut ProgrammeBuilder>,
    document: &mut XmltvDocument,
    offset: u64,
) {
    let language = capture.attributes.get("lang").cloned();
    let text = capture.text.trim().to_owned();
    if let Some(channel) = channel {
        match capture.name.as_str() {
            "display-name" => channel
                .display_names
                .push(LocalizedText::new(text, language)),
            "url" => match Url::parse(&text) {
                Ok(url) => channel.urls.push(url),
                Err(error) => push_diagnostic(
                    document,
                    Diagnostic::warning(
                        DiagnosticCode::InvalidUrl,
                        None,
                        Some(offset),
                        format!("invalid XMLTV channel URL: {error}"),
                    ),
                ),
            },
            _ => preserve_extension(capture, &mut channel.extensions, document, offset),
        }
        return;
    }
    let Some(programme) = programme else {
        return;
    };
    match capture.name.as_str() {
        "title" => programme.titles.push(LocalizedText::new(text, language)),
        "sub-title" => programme
            .sub_titles
            .push(LocalizedText::new(text, language)),
        "desc" => programme
            .descriptions
            .push(LocalizedText::new(text, language)),
        "category" => programme
            .categories
            .push(LocalizedText::new(text, language)),
        "episode-num" => programme.episode_numbers.push(EpisodeNumber {
            value: text,
            system: capture.attributes.get("system").cloned(),
        }),
        "credits" => {
            programme
                .credits
                .entries
                .extend(capture.children.into_iter().filter_map(|entry| {
                    let name = entry.text.trim().to_owned();
                    (!name.is_empty()).then(|| Credit {
                        role: entry.name,
                        name,
                        character: entry.attributes.get("role").cloned(),
                    })
                }));
        }
        "rating" => {
            let value = capture
                .children
                .iter()
                .find(|entry| entry.name == "value")
                .map_or(text, |entry| entry.text.trim().to_owned());
            if !value.is_empty() {
                let icons = capture
                    .children
                    .iter()
                    .filter(|entry| entry.name == "icon")
                    .filter_map(|entry| parse_icon(&entry.attributes, document, offset))
                    .collect();
                programme.ratings.push(ProgrammeRating {
                    value,
                    system: capture.attributes.get("system").cloned(),
                    icons,
                });
            }
        }
        "previously-shown" => programme.previously_shown = true,
        "new" => programme.new = true,
        _ => preserve_extension(capture, &mut programme.extensions, document, offset),
    }
}

fn preserve_extension(
    capture: Capture,
    extensions: &mut Vec<ExtensionElement>,
    document: &mut XmltvDocument,
    offset: u64,
) {
    document.stats.unknown_metadata += 1;
    push_diagnostic(
        document,
        Diagnostic::warning(
            DiagnosticCode::UnknownXmlElement,
            None,
            Some(offset),
            format!("preserved unknown XMLTV element <{}>", capture.name),
        ),
    );
    extensions.push(ExtensionElement {
        name: capture.name,
        attributes: capture.attributes,
        text: capture.text.trim().to_owned(),
    });
}

fn parse_icon(
    attributes: &BTreeMap<String, String>,
    document: &mut XmltvDocument,
    offset: u64,
) -> Option<EpgIcon> {
    let source = attributes.get("src")?;
    let source = match Url::parse(source) {
        Ok(source) => source,
        Err(error) => {
            push_diagnostic(
                document,
                Diagnostic::warning(
                    DiagnosticCode::InvalidUrl,
                    None,
                    Some(offset),
                    format!("invalid XMLTV icon URL: {error}"),
                ),
            );
            return None;
        }
    };
    Some(EpgIcon {
        source,
        width: attributes.get("width").and_then(|value| value.parse().ok()),
        height: attributes
            .get("height")
            .and_then(|value| value.parse().ok()),
    })
}

fn finish_channel(
    builder: ChannelBuilder,
    document: &mut XmltvDocument,
    declared: &mut HashSet<String>,
    offset: u64,
) -> Option<EpgChannel> {
    let Some(id) = builder.id.filter(|id| !id.trim().is_empty()) else {
        invalid_record(document, offset, "XMLTV channel is missing required id");
        return None;
    };
    if !declared.insert(id.clone()) {
        push_diagnostic(
            document,
            Diagnostic::warning(
                DiagnosticCode::DuplicateIdentity,
                None,
                Some(offset),
                format!("duplicate XMLTV channel id {id:?}; metadata fragments will be merged"),
            ),
        );
    }
    let channel = EpgChannel {
        id,
        display_names: builder.display_names,
        icons: builder.icons,
        urls: builder.urls,
        extensions: builder.extensions,
    };
    document.stats.records_emitted += 1;
    Some(channel)
}

fn merge_channel(existing: &mut EpgChannel, duplicate: EpgChannel) {
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

fn visit_unique_programme<FP>(
    programme: Programme,
    document: &mut XmltvDocument,
    fingerprints: &mut HashSet<[u8; 32]>,
    offset: u64,
    visit: &mut FP,
) -> Result<(), ParseError>
where
    FP: FnMut(Programme) -> Result<(), ParseError>,
{
    let serialized = serde_json::to_vec(&programme)
        .map_err(|error| ParseError::Io(std::io::Error::other(error)))?;
    let fingerprint: [u8; 32] = Sha256::digest(serialized).into();
    if !fingerprints.insert(fingerprint) {
        document.stats.records_emitted = document.stats.records_emitted.saturating_sub(1);
        document.stats.records_skipped += 1;
        push_diagnostic(
            document,
            Diagnostic::warning(
                DiagnosticCode::DuplicateIdentity,
                None,
                Some(offset),
                "skipped exact duplicate XMLTV programme",
            ),
        );
        return Ok(());
    }
    visit(programme)
}

fn finish_programme(
    builder: ProgrammeBuilder,
    document: &mut XmltvDocument,
    offset: u64,
    default_timezone: &str,
) -> Option<Programme> {
    let Some(channel_id) = builder.channel_id.filter(|id| !id.trim().is_empty()) else {
        invalid_record(
            document,
            offset,
            "XMLTV programme is missing required channel",
        );
        return None;
    };
    let Some(start_raw) = builder.start else {
        invalid_record(
            document,
            offset,
            "XMLTV programme is missing required start",
        );
        return None;
    };
    let start = match parse_xmltv_timestamp_in_timezone(&start_raw, default_timezone) {
        Ok(value) => value.utc,
        Err(error) => {
            invalid_record(
                document,
                offset,
                format!("invalid programme start: {error}"),
            );
            return None;
        }
    };
    let stop = match builder.stop {
        Some(raw) => match parse_xmltv_timestamp_in_timezone(&raw, default_timezone) {
            Ok(value) => Some(value.utc),
            Err(error) => {
                invalid_record(document, offset, format!("invalid programme stop: {error}"));
                return None;
            }
        },
        None => None,
    };
    if stop.is_some_and(|stop| stop <= start) {
        invalid_record(document, offset, "XMLTV programme stop must be after start");
        return None;
    }
    let programme = Programme {
        channel_id,
        start,
        stop,
        titles: builder.titles,
        sub_titles: builder.sub_titles,
        descriptions: builder.descriptions,
        categories: builder.categories,
        episode_numbers: builder.episode_numbers,
        icons: builder.icons,
        ratings: builder.ratings,
        credits: builder.credits,
        previously_shown: builder.previously_shown,
        new: builder.new,
        extensions: builder.extensions,
    };
    document.stats.records_emitted += 1;
    Some(programme)
}

fn invalid_record(document: &mut XmltvDocument, offset: u64, message: impl Into<String>) {
    document.stats.records_skipped += 1;
    push_diagnostic(
        document,
        Diagnostic::error(
            DiagnosticCode::InvalidXmltvRecord,
            None,
            Some(offset),
            message,
        ),
    );
}

fn push_diagnostic(document: &mut XmltvDocument, diagnostic: Diagnostic) {
    document.stats.observe(&diagnostic);
    document.diagnostics.push(diagnostic);
}

/// Parses XMLTV's compact, optionally partial timestamp and normalizes it to UTC.
/// Missing timezone information follows the XMLTV convention and is treated as UTC.
///
/// # Errors
///
/// Returns [`XmltvTimestampError`] for invalid precision, calendar values, offsets,
/// timezone names, and ambiguous or nonexistent local wall-clock values.
#[allow(clippy::too_many_lines)]
pub fn parse_xmltv_timestamp(value: &str) -> Result<XmltvTimestamp, XmltvTimestampError> {
    parse_xmltv_timestamp_in_timezone(value, "UTC")
}

#[allow(clippy::missing_errors_doc, clippy::too_many_lines)]
pub fn parse_xmltv_timestamp_in_timezone(
    value: &str,
    default_timezone: &str,
) -> Result<XmltvTimestamp, XmltvTimestampError> {
    let original = value.to_owned();
    let value = value.trim();
    if value.is_empty() {
        return Err(XmltvTimestampError::Empty);
    }
    let mut parts = value.split_whitespace();
    let digits = parts.next().ok_or(XmltvTimestampError::Empty)?;
    if !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(XmltvTimestampError::InvalidDigits(digits.to_owned()));
    }
    let precision = match digits.len() {
        4 => TimestampPrecision::Year,
        6 => TimestampPrecision::Month,
        8 => TimestampPrecision::Day,
        10 => TimestampPrecision::Hour,
        12 => TimestampPrecision::Minute,
        14 => TimestampPrecision::Second,
        length => return Err(XmltvTimestampError::UnsupportedPrecision(length)),
    };
    let parse = |range: std::ops::Range<usize>, default: u32| -> Result<u32, XmltvTimestampError> {
        if digits.len() < range.end {
            return Ok(default);
        }
        digits[range]
            .parse()
            .map_err(|_| XmltvTimestampError::InvalidDigits(digits.to_owned()))
    };
    let year: i32 = digits[0..4]
        .parse()
        .map_err(|_| XmltvTimestampError::InvalidDigits(digits.to_owned()))?;
    let month = parse(4..6, 1)?;
    let day = parse(6..8, 1)?;
    let hour = parse(8..10, 0)?;
    let minute = parse(10..12, 0)?;
    let second = parse(12..14, 0)?;
    let naive = NaiveDate::from_ymd_opt(year, month, day)
        .and_then(|date| date.and_hms_opt(hour, minute, second))
        .ok_or_else(|| XmltvTimestampError::InvalidDigits(digits.to_owned()))?;

    let timezone = parts.next();
    if parts.next().is_some() {
        return Err(XmltvTimestampError::InvalidTimezone(
            value[digits.len()..].trim().to_owned(),
        ));
    }
    let timezone_was_implicit = timezone.is_none();
    let effective_timezone = timezone.unwrap_or(default_timezone);
    let (utc, offset_seconds) = match effective_timezone {
        "Z" | "UTC" | "GMT" => (Utc.from_utc_datetime(&naive), 0),
        zone if zone.starts_with(['+', '-']) => {
            let compact = zone.replace(':', "");
            if compact.len() != 5 || !compact[1..].bytes().all(|byte| byte.is_ascii_digit()) {
                return Err(XmltvTimestampError::InvalidTimezone(zone.to_owned()));
            }
            let hours: i32 = compact[1..3]
                .parse()
                .map_err(|_| XmltvTimestampError::InvalidTimezone(zone.to_owned()))?;
            let minutes: i32 = compact[3..5]
                .parse()
                .map_err(|_| XmltvTimestampError::InvalidTimezone(zone.to_owned()))?;
            if hours > 23 || minutes > 59 {
                return Err(XmltvTimestampError::InvalidTimezone(zone.to_owned()));
            }
            let sign = if compact.starts_with('-') { -1 } else { 1 };
            let seconds = sign * (hours * 3600 + minutes * 60);
            let offset = FixedOffset::east_opt(seconds)
                .ok_or_else(|| XmltvTimestampError::InvalidTimezone(zone.to_owned()))?;
            let LocalResult::Single(local) = offset.from_local_datetime(&naive) else {
                return Err(XmltvTimestampError::InvalidTimezone(zone.to_owned()));
            };
            (local.with_timezone(&Utc), seconds)
        }
        zone => {
            let timezone = Tz::from_str(zone)
                .map_err(|_| XmltvTimestampError::InvalidTimezone(zone.to_owned()))?;
            match timezone.from_local_datetime(&naive) {
                LocalResult::Single(value) => {
                    let offset = value.offset().fix().local_minus_utc();
                    (value.with_timezone(&Utc), offset)
                }
                LocalResult::Ambiguous(_, _) => {
                    return Err(XmltvTimestampError::Ambiguous {
                        value: digits.to_owned(),
                        timezone: zone.to_owned(),
                    });
                }
                LocalResult::None => {
                    return Err(XmltvTimestampError::Nonexistent {
                        value: digits.to_owned(),
                        timezone: zone.to_owned(),
                    });
                }
            }
        }
    };
    Ok(XmltvTimestamp {
        utc,
        precision,
        offset_seconds,
        timezone_was_implicit,
        original,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use std::io::Cursor;

    const FIXTURE: &str = include_str!("../../../tests/fixtures/xmltv/guide.xml");

    #[test]
    fn parses_channels_programmes_extensions_and_common_metadata() {
        let document = parse_xmltv(Cursor::new(FIXTURE), ParseLimits::default()).expect("parse");
        assert_eq!(document.channels.len(), 2);
        assert_eq!(document.programmes.len(), 2);
        assert_eq!(document.channels[0].id, "kusa.denver.example");
        assert_eq!(document.programmes[0].titles[0].value, "Broncos vs Chiefs");
        assert_eq!(document.programmes[0].categories[0].value, "Football");
        assert_eq!(
            document.programmes[0].credits.entries[0].role,
            "commentator"
        );
        assert_eq!(document.programmes[0].ratings[0].value, "TV-PG");
        assert!(document.programmes[0].new);
        assert!(
            document.programmes[0]
                .extensions
                .iter()
                .any(|item| item.name == "provider-data")
        );
        assert!(
            document
                .diagnostics
                .iter()
                .any(|item| item.code == DiagnosticCode::UnknownXmlElement)
        );
    }

    #[test]
    fn timestamps_normalize_offsets_and_default_missing_zone_to_utc() {
        let offset = parse_xmltv_timestamp("20260913182000 -0600").expect("offset");
        assert_eq!(offset.utc.to_rfc3339(), "2026-09-14T00:20:00+00:00");
        assert_eq!(offset.offset_seconds, -21_600);
        assert!(!offset.timezone_was_implicit);

        let partial = parse_xmltv_timestamp("20260913").expect("partial");
        assert_eq!(partial.precision, TimestampPrecision::Day);
        assert_eq!(partial.utc.to_rfc3339(), "2026-09-13T00:00:00+00:00");
        assert!(partial.timezone_was_implicit);
    }

    #[test]
    fn invalid_and_ambiguous_times_fail_closed() {
        assert!(matches!(
            parse_xmltv_timestamp("20260230010000 +0000"),
            Err(XmltvTimestampError::InvalidDigits(_))
        ));
        assert!(matches!(
            parse_xmltv_timestamp("20261101013000 America/Denver"),
            Err(XmltvTimestampError::Ambiguous { .. })
        ));
    }

    #[test]
    fn configured_default_timezone_normalizes_and_rejects_dst_edges() {
        let denver = parse_xmltv_timestamp_in_timezone("20260101120000", "America/Denver")
            .expect("unambiguous winter local time");
        assert_eq!(denver.utc.to_rfc3339(), "2026-01-01T19:00:00+00:00");
        assert_eq!(denver.offset_seconds, -25_200);
        assert!(denver.timezone_was_implicit);

        assert!(matches!(
            parse_xmltv_timestamp_in_timezone("20260308023000", "America/Denver"),
            Err(XmltvTimestampError::Nonexistent { .. })
        ));
        assert!(matches!(
            parse_xmltv_timestamp_in_timezone("20261101013000", "America/Denver"),
            Err(XmltvTimestampError::Ambiguous { .. })
        ));

        let options = XmltvParseOptions {
            default_timezone: "America/Denver".to_owned(),
        };
        let document = parse_xmltv_with_options(
            Cursor::new(
                r#"<tv><channel id="a"/><programme channel="a" start="20260101120000"><title>Noon</title></programme></tv>"#,
            ),
            ParseLimits::default(),
            &options,
        )
        .expect("guide in configured timezone");
        assert_eq!(
            document.programmes[0].start.to_rfc3339(),
            "2026-01-01T19:00:00+00:00"
        );
    }

    #[test]
    fn accepts_external_xmltv_doctype_and_rejects_unsafe_declarations() {
        let xml = "<!DOCTYPE tv SYSTEM \"http://example.test/xmltv.dtd\"><tv/>";
        assert!(parse_xmltv(Cursor::new(xml), ParseLimits::default()).is_ok());

        for xml in [
            "<!DOCTYPE html SYSTEM \"http://example.test/xmltv.dtd\"><tv/>",
            "<!DOCTYPE tv [<!ENTITY x \"value\">]><tv/>",
        ] {
            assert!(matches!(
                parse_xmltv(Cursor::new(xml), ParseLimits::default()),
                Err(ParseError::XmlDoctypeForbidden)
            ));
        }
    }

    #[test]
    fn rejects_oversized_input() {
        let limits = ParseLimits {
            max_input_bytes: 10,
            ..ParseLimits::default()
        };
        assert!(matches!(
            parse_xmltv(Cursor::new("<tv>long content</tv>"), limits),
            Err(ParseError::InputTooLarge { limit: 10 })
        ));
    }

    #[test]
    fn invalid_records_are_skipped_and_cross_references_are_diagnosed() {
        let xml = r#"<tv>
          <channel id="duplicate"><display-name>One</display-name></channel>
          <channel id="duplicate"><display-name>Two</display-name></channel>
          <programme channel="duplicate" start="20260101120000 +0000" stop="20260101110000 +0000"><title>Backwards</title></programme>
          <programme channel="missing" start="20260101120000 +0000"><title>Valid but orphaned</title></programme>
        </tv>"#;
        let document = parse_xmltv(Cursor::new(xml), ParseLimits::default()).expect("parse");
        assert_eq!(document.channels.len(), 1);
        assert_eq!(document.programmes.len(), 1);
        assert_eq!(document.stats.records_skipped, 1);
        assert!(document.diagnostics.iter().any(|item| {
            item.code == DiagnosticCode::DuplicateIdentity && item.message.contains("duplicate")
        }));
        assert!(
            document
                .diagnostics
                .iter()
                .any(|item| item.code == DiagnosticCode::UndeclaredChannel)
        );
    }

    #[test]
    fn duplicate_channels_merge_distinct_metadata_in_source_order() {
        let xml = r#"<tv>
          <channel id="a">
            <display-name lang="en">Alpha</display-name>
            <icon src="https://img.test/a.png" width="100"/>
            <url>https://one.test/channel</url>
            <provider-tag value="one">first</provider-tag>
          </channel>
          <channel id="a">
            <display-name lang="en">Alpha</display-name>
            <display-name lang="es">Alfa</display-name>
            <icon src="https://img.test/a.png" width="100"/>
            <icon src="https://img.test/b.png"/>
            <url>https://one.test/channel</url>
            <url>https://two.test/channel</url>
            <provider-tag value="two">second</provider-tag>
          </channel>
        </tv>"#;
        let document = parse_xmltv(Cursor::new(xml), ParseLimits::default()).expect("parse");
        assert_eq!(document.channels.len(), 1);
        let channel = &document.channels[0];
        assert_eq!(
            channel
                .display_names
                .iter()
                .map(|name| name.value.as_str())
                .collect::<Vec<_>>(),
            ["Alpha", "Alfa"]
        );
        assert_eq!(channel.icons.len(), 2);
        assert_eq!(channel.urls.len(), 2);
        assert_eq!(channel.extensions.len(), 2);
        assert!(
            document
                .diagnostics
                .iter()
                .any(|item| item.code == DiagnosticCode::DuplicateIdentity)
        );
    }

    #[test]
    fn exact_programmes_are_deduplicated_but_conflicting_overlaps_remain() {
        let xml = r#"<tv>
          <channel id="a"/>
          <programme channel="a" start="20260101120000 +0000" stop="20260101130000 +0000"><title>Game</title><desc>Feed one</desc></programme>
          <programme channel="a" start="20260101120000 +0000" stop="20260101130000 +0000"><title>Game</title><desc>Feed one</desc></programme>
          <programme channel="a" start="20260101120000 +0000" stop="20260101130000 +0000"><title>Game</title><desc>Feed two</desc></programme>
        </tv>"#;
        let document = parse_xmltv(Cursor::new(xml), ParseLimits::default()).expect("parse");
        assert_eq!(document.programmes.len(), 2);
        assert_eq!(document.stats.records_seen, 4);
        assert_eq!(document.stats.records_emitted, 3);
        assert_eq!(document.stats.records_skipped, 1);
        assert_eq!(document.programmes[0].descriptions[0].value, "Feed one");
        assert_eq!(document.programmes[1].descriptions[0].value, "Feed two");
        assert!(
            document
                .diagnostics
                .iter()
                .any(|item| item.code == DiagnosticCode::DuplicateIdentity)
        );
    }

    #[test]
    fn enforces_xml_record_text_and_depth_limits() {
        let record_limits = ParseLimits {
            max_records: 1,
            ..ParseLimits::default()
        };
        assert!(matches!(
            parse_xmltv(
                Cursor::new("<tv><channel id=\"a\"/><channel id=\"b\"/></tv>"),
                record_limits
            ),
            Err(ParseError::TooManyRecords { limit: 1 })
        ));

        let text_limits = ParseLimits {
            max_text_bytes: 3,
            ..ParseLimits::default()
        };
        assert!(matches!(
            parse_xmltv(
                Cursor::new(
                    "<tv><channel id=\"a\"><display-name>long</display-name></channel></tv>"
                ),
                text_limits
            ),
            Err(ParseError::TextTooLarge { limit: 3 })
        ));

        let depth_limits = ParseLimits {
            max_xml_depth: 2,
            ..ParseLimits::default()
        };
        assert!(matches!(
            parse_xmltv(
                Cursor::new("<tv><channel id=\"a\"><display-name>x</display-name></channel></tv>"),
                depth_limits
            ),
            Err(ParseError::XmlTooDeep { limit: 2 })
        ));
    }

    #[test]
    fn visitor_output_is_equivalent_and_not_retained_by_summary() {
        let collected = parse_xmltv(Cursor::new(FIXTURE), ParseLimits::default()).expect("collect");
        let mut channels = Vec::new();
        let mut programmes = Vec::new();
        let summary = parse_xmltv_visit(
            Cursor::new(FIXTURE),
            ParseLimits::default(),
            |channel| {
                channels.push(channel);
                Ok(())
            },
            |programme| {
                programmes.push(programme);
                Ok(())
            },
        )
        .expect("visit");
        assert!(summary.channels.is_empty());
        assert!(summary.programmes.is_empty());
        assert_eq!(channels, collected.channels);
        assert_eq!(programmes, collected.programmes);
        assert_eq!(summary.generator, collected.generator);
        assert_eq!(summary.diagnostics, collected.diagnostics);
        assert_eq!(summary.stats, collected.stats);
    }

    proptest! {
        #[test]
        fn numeric_offsets_round_trip_to_expected_instant(
            year in 2000_i32..2099,
            month in 1_u32..13,
            day in 1_u32..29,
            hour in 0_u32..24,
            minute in 0_u32..60,
            offset_hours in -12_i32..15,
        ) {
            let sign = if offset_hours < 0 { '-' } else { '+' };
            let source = format!("{year:04}{month:02}{day:02}{hour:02}{minute:02}00 {sign}{:02}00", offset_hours.abs());
            let parsed = parse_xmltv_timestamp(&source).expect("valid generated timestamp");
            let expected_offset = FixedOffset::east_opt(offset_hours * 3600).unwrap();
            let expected = expected_offset
                .with_ymd_and_hms(year, month, day, hour, minute, 0)
                .unwrap()
                .with_timezone(&Utc);
            prop_assert_eq!(parsed.utc, expected);
        }
    }
}
