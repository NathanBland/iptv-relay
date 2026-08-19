use std::io;
use thiserror::Error;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Severity {
    Warning,
    Error,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiagnosticCode {
    MissingHeader,
    OrphanMetadata,
    OrphanUrl,
    ReplacedPendingEntry,
    InvalidAttribute,
    InvalidDuration,
    InvalidUrl,
    UnknownDirective,
    InvalidXmltvRecord,
    UnknownXmlElement,
    InvalidTimestamp,
    InvalidInterval,
    UndeclaredChannel,
    DuplicateIdentity,
    InvalidEventRule,
    InvalidXtreamRecord,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Diagnostic {
    pub severity: Severity,
    pub code: DiagnosticCode,
    pub line: Option<u64>,
    pub byte_offset: Option<u64>,
    pub message: String,
}

impl Diagnostic {
    pub(crate) fn warning(
        code: DiagnosticCode,
        line: Option<u64>,
        byte_offset: Option<u64>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            severity: Severity::Warning,
            code,
            line,
            byte_offset,
            message: message.into(),
        }
    }

    pub(crate) fn error(
        code: DiagnosticCode,
        line: Option<u64>,
        byte_offset: Option<u64>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            severity: Severity::Error,
            code,
            line,
            byte_offset,
            message: message.into(),
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ParseStats {
    pub bytes_read: u64,
    pub lines_read: u64,
    pub records_seen: u64,
    pub records_emitted: u64,
    pub records_skipped: u64,
    pub unknown_metadata: u64,
    pub warnings: u64,
    pub errors: u64,
}

impl ParseStats {
    pub(crate) fn observe(&mut self, diagnostic: &Diagnostic) {
        match diagnostic.severity {
            Severity::Warning => self.warnings += 1,
            Severity::Error => self.errors += 1,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ParseLimits {
    pub max_input_bytes: u64,
    pub max_line_bytes: usize,
    pub max_records: usize,
    pub max_text_bytes: usize,
    pub max_xml_depth: usize,
}

impl Default for ParseLimits {
    fn default() -> Self {
        Self {
            max_input_bytes: 64 * 1024 * 1024,
            max_line_bytes: 64 * 1024,
            max_records: 1_000_000,
            max_text_bytes: 1024 * 1024,
            max_xml_depth: 64,
        }
    }
}

#[derive(Debug, Error)]
pub enum ParseError {
    #[error("I/O error while parsing: {0}")]
    Io(#[from] io::Error),
    #[error("input exceeds configured limit of {limit} bytes")]
    InputTooLarge { limit: u64 },
    #[error("line {line} exceeds configured limit of {limit} bytes")]
    LineTooLong { line: u64, limit: usize },
    #[error("record count exceeds configured limit of {limit}")]
    TooManyRecords { limit: usize },
    #[error("text node exceeds configured limit of {limit} bytes")]
    TextTooLarge { limit: usize },
    #[error("XML nesting exceeds configured depth of {limit}")]
    XmlTooDeep { limit: usize },
    #[error("malformed UTF-8 on line {line}")]
    InvalidUtf8 { line: u64 },
    #[error("malformed M3U: {0}")]
    MalformedM3u(String),
    #[error("malformed XMLTV at byte {offset}: {message}")]
    MalformedXmltv { offset: u64, message: String },
    #[error("XML document types are disabled")]
    XmlDoctypeForbidden,
    #[error("malformed event rules: {0}")]
    MalformedEventRules(String),
    #[error("malformed Xtream Codes response: {0}")]
    MalformedXtream(String),
}
