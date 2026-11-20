//! Records the lifecycle of each viewer stream session.
//!
//! Each `/out` and preview stream gets one record. The record holds the
//! channel, the container, the provider stream, the start and end times, the
//! bytes sent, and the end reason. The relay logs each record at the info
//! level and keeps the most recent records in a bounded ring. The
//! `/api/v1/support/stream-sessions` route returns the ring.
//!
//! A record never holds an output token, a credential, or an upstream URL.
//! Each detail message goes through [`redact_error`].

use std::{
    collections::VecDeque,
    fmt,
    pin::Pin,
    sync::{Arc, Mutex, PoisonError},
    task::{Context, Poll},
    time::Instant,
};

use bytes::Bytes;
use futures_util::{Stream, StreamExt};
use iptv_domain::Timestamp;
use iptv_media::{
    RemuxExit, RemuxInputEnd, RemuxOutputEnd, RemuxReport, RemuxSide, RemuxSummary,
    ViewerStreamError,
};
use iptv_persistence::redact_error;
use serde::Serialize;
use tracing::info;
use utoipa::ToSchema;
use uuid::Uuid;

/// The number of ended stream sessions that the ring keeps.
pub(crate) const STREAM_SESSION_HISTORY: usize = 200;
/// The longest detail message in a record, in characters.
const MAX_DETAIL_CHARS: usize = 512;

/// Why a viewer stream session ended.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum StreamSessionEndReason {
    /// The client closed the connection before the stream ended.
    ClientDisconnect,
    /// The upstream stream ended without an error.
    UpstreamEnded,
    /// The upstream stream failed.
    UpstreamError,
    /// Upstream recovery stopped after its retry limit.
    RecoveryExpired,
    /// The client fell behind the live buffer.
    ViewerLagged,
    /// The upstream session restarted with a new stream generation.
    StreamRestarted,
    /// A read from the live buffer failed.
    BufferError,
    /// The input of the Matroska remux failed.
    RemuxInputError,
    /// The `FFmpeg` remux process stopped before its input ended.
    RemuxExited,
    /// A read from the Matroska remux failed.
    RemuxOutputError,
    /// The stream did not start.
    StartFailed,
}

impl StreamSessionEndReason {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::ClientDisconnect => "client_disconnect",
            Self::UpstreamEnded => "upstream_ended",
            Self::UpstreamError => "upstream_error",
            Self::RecoveryExpired => "recovery_expired",
            Self::ViewerLagged => "viewer_lagged",
            Self::StreamRestarted => "stream_restarted",
            Self::BufferError => "buffer_error",
            Self::RemuxInputError => "remux_input_error",
            Self::RemuxExited => "remux_exited",
            Self::RemuxOutputError => "remux_output_error",
            Self::StartFailed => "start_failed",
        }
    }
}

/// How the Matroska remux of a session ended.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RemuxSessionSummary {
    /// The remux side that ended first: `input` or `output`.
    first_end: Option<String>,
    /// The `FFmpeg` exit code.
    exit_code: Option<i32>,
    /// The signal that stopped `FFmpeg`.
    exit_signal: Option<i32>,
    /// The redacted input error message.
    input_error: Option<String>,
}

impl RemuxSessionSummary {
    fn from_summary(summary: &RemuxSummary) -> Self {
        let (exit_code, exit_signal) = match summary.exit {
            Some(RemuxExit::Code(code)) => (Some(code), None),
            Some(RemuxExit::Signal(signal)) => (None, Some(signal)),
            Some(RemuxExit::Unknown) | None => (None, None),
        };
        let input_error = match &summary.input_end {
            Some(RemuxInputEnd::Failed(message)) => Some(redact_detail(message)),
            _ => None,
        };
        Self {
            first_end: summary.first_end.map(|side| side.as_str().to_owned()),
            exit_code,
            exit_signal,
            input_error,
        }
    }
}

/// One ended viewer stream session.
#[derive(Clone, Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) struct StreamSessionRecord {
    id: Uuid,
    channel_id: Uuid,
    /// The output container: `ts` or `mkv`.
    container: String,
    provider_stream_id: Option<Uuid>,
    /// True when the response started with a slate.
    slated: bool,
    started_at: Timestamp,
    ended_at: Timestamp,
    duration_ms: u64,
    bytes_sent: u64,
    end_reason: StreamSessionEndReason,
    /// The redacted message that goes with the end reason.
    detail: Option<String>,
    /// How the Matroska remux ended. Only `mkv` sessions have this value.
    remux: Option<RemuxSessionSummary>,
}

/// A bounded ring of ended stream sessions.
#[derive(Debug)]
pub(crate) struct StreamSessionLog {
    capacity: usize,
    entries: Mutex<VecDeque<StreamSessionRecord>>,
}

impl Default for StreamSessionLog {
    fn default() -> Self {
        Self::with_capacity(STREAM_SESSION_HISTORY)
    }
}

impl StreamSessionLog {
    pub(crate) fn with_capacity(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            entries: Mutex::new(VecDeque::with_capacity(capacity.max(1))),
        }
    }

    fn push(&self, record: StreamSessionRecord) {
        let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        while entries.len() >= self.capacity {
            entries.pop_front();
        }
        entries.push_back(record);
    }

    /// Returns the ended sessions, the newest first.
    pub(crate) fn recent(&self) -> Vec<StreamSessionRecord> {
        self.entries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .rev()
            .cloned()
            .collect()
    }
}

/// How the response body ended.
#[derive(Clone, Debug, Eq, PartialEq)]
enum BodyEnd {
    Finished,
    Failed(String),
}

/// The facts that decide the end reason of a session.
#[derive(Clone, Debug, Default)]
struct EndFacts {
    /// `None` when the client dropped the body before it ended.
    body: Option<BodyEnd>,
    /// The first error from the live viewer stream.
    viewer_error: Option<(StreamSessionEndReason, String)>,
    /// The first error from the slated stream.
    slated_error: Option<String>,
    remux: Option<RemuxSummary>,
    start_error: Option<String>,
}

fn classify_viewer_error(error: &ViewerStreamError) -> (StreamSessionEndReason, String) {
    let reason = match error {
        ViewerStreamError::Upstream { .. } => StreamSessionEndReason::UpstreamError,
        ViewerStreamError::RecoveryExpired { .. } => StreamSessionEndReason::RecoveryExpired,
        ViewerStreamError::Lagged { .. } => StreamSessionEndReason::ViewerLagged,
        ViewerStreamError::GenerationChanged { .. } => StreamSessionEndReason::StreamRestarted,
        ViewerStreamError::Ring(_) => StreamSessionEndReason::BufferError,
    };
    (reason, error.to_string())
}

/// Decides the end reason and the unredacted detail of a session.
fn classify(facts: &EndFacts) -> (StreamSessionEndReason, Option<String>) {
    if let Some(error) = &facts.start_error {
        return (StreamSessionEndReason::StartFailed, Some(error.clone()));
    }
    let Some(body) = &facts.body else {
        return (StreamSessionEndReason::ClientDisconnect, None);
    };
    if let Some((reason, message)) = &facts.viewer_error {
        return (*reason, Some(message.clone()));
    }
    if let Some(message) = &facts.slated_error {
        return (StreamSessionEndReason::UpstreamError, Some(message.clone()));
    }
    if let Some(remux) = &facts.remux {
        if let Some(RemuxInputEnd::Failed(message)) = &remux.input_end {
            return (
                StreamSessionEndReason::RemuxInputError,
                Some(message.clone()),
            );
        }
        if let Some(RemuxOutputEnd::Failed(message)) = &remux.output_end {
            return (
                StreamSessionEndReason::RemuxOutputError,
                Some(message.clone()),
            );
        }
        let exit = remux.exit.unwrap_or(RemuxExit::Unknown);
        return match (remux.first_end, &remux.input_end) {
            (Some(RemuxSide::Input), Some(RemuxInputEnd::Finished)) => {
                (StreamSessionEndReason::UpstreamEnded, None)
            }
            _ => (
                StreamSessionEndReason::RemuxExited,
                Some(format!("FFmpeg ended before its input ({exit})")),
            ),
        };
    }
    match body {
        BodyEnd::Finished => (StreamSessionEndReason::UpstreamEnded, None),
        BodyEnd::Failed(message) => (StreamSessionEndReason::UpstreamError, Some(message.clone())),
    }
}

fn redact_detail(message: &str) -> String {
    redact_error(message)
        .chars()
        .take(MAX_DETAIL_CHARS)
        .collect()
}

struct TrackerState {
    facts: EndFacts,
    slated: bool,
    remux: Option<RemuxReport>,
    ended: Option<(Instant, Timestamp)>,
    bytes_sent: u64,
    recorded: bool,
}

struct TrackerShared {
    log: Arc<StreamSessionLog>,
    id: Uuid,
    channel_id: Uuid,
    container: &'static str,
    provider_stream_id: Option<Uuid>,
    started_at: Timestamp,
    started: Instant,
    state: Mutex<TrackerState>,
}

impl TrackerShared {
    fn lock(&self) -> std::sync::MutexGuard<'_, TrackerState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Logs and stores the record once.
    fn finish(&self) {
        let record = {
            let mut state = self.lock();
            if state.recorded {
                return;
            }
            state.recorded = true;
            let remux_summary = state.remux.as_ref().map(RemuxReport::summary);
            let mut facts = state.facts.clone();
            facts.remux.clone_from(&remux_summary);
            let (end_reason, detail) = classify(&facts);
            let (ended, ended_at) = state
                .ended
                .unwrap_or_else(|| (Instant::now(), Timestamp::now()));
            StreamSessionRecord {
                id: self.id,
                channel_id: self.channel_id,
                container: self.container.to_owned(),
                provider_stream_id: self.provider_stream_id,
                slated: state.slated,
                started_at: self.started_at,
                ended_at,
                duration_ms: u64::try_from(ended.duration_since(self.started).as_millis())
                    .unwrap_or(u64::MAX),
                bytes_sent: state.bytes_sent,
                end_reason,
                detail: detail.as_deref().map(redact_detail),
                remux: remux_summary
                    .as_ref()
                    .map(RemuxSessionSummary::from_summary),
            }
        };
        let remux = record.remux.as_ref();
        info!(
            session_id = %record.id,
            channel_id = %record.channel_id,
            container = %record.container,
            provider_stream_id = ?record.provider_stream_id,
            slated = record.slated,
            started_at = %record.started_at.to_rfc3339_z(),
            ended_at = %record.ended_at.to_rfc3339_z(),
            duration_ms = record.duration_ms,
            bytes_sent = record.bytes_sent,
            end_reason = record.end_reason.as_str(),
            detail = record.detail.as_deref(),
            remux_first_end = remux.and_then(|remux| remux.first_end.as_deref()),
            remux_exit_code = remux.and_then(|remux| remux.exit_code),
            remux_exit_signal = remux.and_then(|remux| remux.exit_signal),
            remux_input_error = remux.and_then(|remux| remux.input_error.as_deref()),
            "stream session ended"
        );
        self.log.push(record);
    }
}

/// Follows one viewer stream session from its start to its end.
#[derive(Clone)]
pub(crate) struct StreamSessionTracker {
    shared: Arc<TrackerShared>,
}

impl fmt::Debug for StreamSessionTracker {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StreamSessionTracker")
            .field("id", &self.shared.id)
            .field("channel_id", &self.shared.channel_id)
            .finish_non_exhaustive()
    }
}

impl StreamSessionTracker {
    /// Starts a session record and logs the start.
    pub(crate) fn start(
        log: Arc<StreamSessionLog>,
        channel_id: Uuid,
        container: &'static str,
        provider_stream_id: Option<Uuid>,
    ) -> Self {
        let shared = TrackerShared {
            log,
            id: Uuid::new_v4(),
            channel_id,
            container,
            provider_stream_id,
            started_at: Timestamp::now(),
            started: Instant::now(),
            state: Mutex::new(TrackerState {
                facts: EndFacts::default(),
                slated: false,
                remux: None,
                ended: None,
                bytes_sent: 0,
                recorded: false,
            }),
        };
        info!(
            session_id = %shared.id,
            channel_id = %channel_id,
            container,
            provider_stream_id = ?provider_stream_id,
            "stream session started"
        );
        Self {
            shared: Arc::new(shared),
        }
    }

    /// Records the first error of a live viewer stream.
    pub(crate) fn tap_viewer<S>(
        &self,
        stream: S,
    ) -> impl Stream<Item = Result<Bytes, ViewerStreamError>> + Send + Unpin + 'static
    where
        S: Stream<Item = Result<Bytes, ViewerStreamError>> + Send + Unpin + 'static,
    {
        let shared = Arc::clone(&self.shared);
        stream.inspect(move |item| {
            if let Err(error) = item {
                shared
                    .lock()
                    .facts
                    .viewer_error
                    .get_or_insert_with(|| classify_viewer_error(error));
            }
        })
    }

    /// Records that the response starts with a slate, and records the first
    /// error of the slated stream.
    pub(crate) fn tap_slated<S>(
        &self,
        stream: S,
    ) -> impl Stream<Item = Result<Bytes, std::io::Error>> + Send + Unpin + 'static
    where
        S: Stream<Item = Result<Bytes, std::io::Error>> + Send + Unpin + 'static,
    {
        self.shared.lock().slated = true;
        let shared = Arc::clone(&self.shared);
        stream.inspect(move |item| {
            if let Err(error) = item {
                shared
                    .lock()
                    .facts
                    .slated_error
                    .get_or_insert_with(|| error.to_string());
            }
        })
    }

    /// Attaches the report of the Matroska remux.
    pub(crate) fn set_remux(&self, report: RemuxReport) {
        self.shared.lock().remux = Some(report);
    }

    /// Records a session that did not start.
    pub(crate) fn fail(self, message: &str) {
        self.shared.lock().facts.start_error = Some(message.to_owned());
        self.shared.finish();
    }

    /// Wraps the response body. The record ends when the body ends or drops.
    pub(crate) fn body<S, E>(self, stream: S) -> TrackedBody<S>
    where
        S: Stream<Item = Result<Bytes, E>> + Unpin,
        E: fmt::Display,
    {
        TrackedBody {
            inner: stream,
            shared: self.shared,
        }
    }
}

/// A response body that counts bytes and records how it ended.
pub(crate) struct TrackedBody<S> {
    inner: S,
    shared: Arc<TrackerShared>,
}

impl<S> TrackedBody<S> {
    fn end(&self, end: BodyEnd) {
        let mut state = self.shared.lock();
        if state.facts.body.is_none() {
            state.facts.body = Some(end);
            state.ended = Some((Instant::now(), Timestamp::now()));
        }
    }
}

impl<S, E> Stream for TrackedBody<S>
where
    S: Stream<Item = Result<Bytes, E>> + Unpin,
    E: fmt::Display,
{
    type Item = Result<Bytes, E>;

    fn poll_next(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let item = Pin::new(&mut self.inner).poll_next(context);
        match &item {
            Poll::Ready(Some(Ok(bytes))) => {
                let length = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
                let mut state = self.shared.lock();
                state.bytes_sent = state.bytes_sent.saturating_add(length);
            }
            Poll::Ready(Some(Err(error))) => self.end(BodyEnd::Failed(error.to_string())),
            Poll::Ready(None) => self.end(BodyEnd::Finished),
            Poll::Pending => {}
        }
        item
    }
}

impl<S> Drop for TrackedBody<S> {
    fn drop(&mut self) {
        self.shared.finish();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::stream;

    fn remux(
        first_end: RemuxSide,
        input_end: RemuxInputEnd,
        output_end: RemuxOutputEnd,
        exit: RemuxExit,
    ) -> RemuxSummary {
        RemuxSummary {
            input_end: Some(input_end),
            output_end: Some(output_end),
            exit: Some(exit),
            first_end: Some(first_end),
        }
    }

    #[test]
    fn a_dropped_body_is_a_client_disconnect() {
        let facts = EndFacts::default();
        assert_eq!(
            classify(&facts),
            (StreamSessionEndReason::ClientDisconnect, None)
        );
    }

    #[test]
    fn viewer_errors_map_to_their_reasons() {
        let cases = [
            (
                ViewerStreamError::Upstream {
                    message: "connection reset".into(),
                },
                StreamSessionEndReason::UpstreamError,
            ),
            (
                ViewerStreamError::RecoveryExpired { attempts: 4 },
                StreamSessionEndReason::RecoveryExpired,
            ),
            (
                ViewerStreamError::Lagged { skipped_packets: 9 },
                StreamSessionEndReason::ViewerLagged,
            ),
            (
                ViewerStreamError::GenerationChanged {
                    previous_generation: 1,
                    generation: 2,
                },
                StreamSessionEndReason::StreamRestarted,
            ),
        ];
        for (error, expected) in cases {
            let facts = EndFacts {
                body: Some(BodyEnd::Failed(error.to_string())),
                viewer_error: Some(classify_viewer_error(&error)),
                ..EndFacts::default()
            };
            let (reason, detail) = classify(&facts);
            assert_eq!(reason, expected, "{error}");
            assert_eq!(detail, Some(error.to_string()));
        }
    }

    #[test]
    fn a_viewer_error_wins_over_the_remux_input_error() {
        // The remux input sees the same error as an I/O error.
        let error = ViewerStreamError::RecoveryExpired { attempts: 3 };
        let facts = EndFacts {
            body: Some(BodyEnd::Finished),
            viewer_error: Some(classify_viewer_error(&error)),
            remux: Some(remux(
                RemuxSide::Input,
                RemuxInputEnd::Failed(error.to_string()),
                RemuxOutputEnd::Finished,
                RemuxExit::Code(0),
            )),
            ..EndFacts::default()
        };
        assert_eq!(classify(&facts).0, StreamSessionEndReason::RecoveryExpired);
    }

    #[test]
    fn remux_ends_map_to_their_reasons() {
        let cases = [
            (
                remux(
                    RemuxSide::Input,
                    RemuxInputEnd::Failed("slate failed".to_owned()),
                    RemuxOutputEnd::Finished,
                    RemuxExit::Code(0),
                ),
                StreamSessionEndReason::RemuxInputError,
                Some("slate failed".to_owned()),
            ),
            (
                remux(
                    RemuxSide::Output,
                    RemuxInputEnd::OutputClosed,
                    RemuxOutputEnd::Failed("broken pipe".to_owned()),
                    RemuxExit::Unknown,
                ),
                StreamSessionEndReason::RemuxOutputError,
                Some("broken pipe".to_owned()),
            ),
            (
                remux(
                    RemuxSide::Output,
                    RemuxInputEnd::OutputClosed,
                    RemuxOutputEnd::Finished,
                    RemuxExit::Code(1),
                ),
                StreamSessionEndReason::RemuxExited,
                Some("FFmpeg ended before its input (exit code 1)".to_owned()),
            ),
            (
                remux(
                    RemuxSide::Output,
                    RemuxInputEnd::OutputClosed,
                    RemuxOutputEnd::Finished,
                    RemuxExit::Signal(9),
                ),
                StreamSessionEndReason::RemuxExited,
                Some("FFmpeg ended before its input (signal 9)".to_owned()),
            ),
            (
                remux(
                    RemuxSide::Input,
                    RemuxInputEnd::Finished,
                    RemuxOutputEnd::Finished,
                    RemuxExit::Code(0),
                ),
                StreamSessionEndReason::UpstreamEnded,
                None,
            ),
        ];
        for (summary, reason, detail) in cases {
            let facts = EndFacts {
                body: Some(BodyEnd::Finished),
                remux: Some(summary.clone()),
                ..EndFacts::default()
            };
            assert_eq!(classify(&facts), (reason, detail), "{summary:?}");
        }
    }

    #[test]
    fn slated_and_start_failures_map_to_their_reasons() {
        let slated = EndFacts {
            body: Some(BodyEnd::Finished),
            slated_error: Some("slate process failed".to_owned()),
            ..EndFacts::default()
        };
        assert_eq!(classify(&slated).0, StreamSessionEndReason::UpstreamError);
        let start = EndFacts {
            start_error: Some("capacity".to_owned()),
            ..EndFacts::default()
        };
        assert_eq!(classify(&start).0, StreamSessionEndReason::StartFailed);
        let clean = EndFacts {
            body: Some(BodyEnd::Finished),
            ..EndFacts::default()
        };
        assert_eq!(classify(&clean).0, StreamSessionEndReason::UpstreamEnded);
    }

    #[test]
    fn details_are_redacted_and_bounded() {
        let message = "upstream failed for http://user:pass@provider.example/live/123.ts?token=abc \
                       password=hunter2 authorization: Bearer xyz";
        let redacted = redact_detail(message);
        for secret in [
            "provider.example",
            "user:pass",
            "abc",
            "hunter2",
            "Bearer xyz",
        ] {
            assert!(!redacted.contains(secret), "{secret} in {redacted}");
        }
        assert!(redacted.contains("[REDACTED_URL]"));
        assert_eq!(redact_detail(&"x".repeat(4_000)).chars().count(), 512);
    }

    #[test]
    fn remux_summary_redacts_the_input_error() {
        let summary = RemuxSessionSummary::from_summary(&remux(
            RemuxSide::Input,
            RemuxInputEnd::Failed("read https://provider.example/a.ts failed".to_owned()),
            RemuxOutputEnd::Finished,
            RemuxExit::Code(0),
        ));
        assert_eq!(summary.first_end.as_deref(), Some("input"));
        assert_eq!(summary.exit_code, Some(0));
        assert_eq!(summary.exit_signal, None);
        let input_error = summary.input_error.expect("input error");
        assert!(!input_error.contains("provider.example"));
    }

    #[test]
    fn the_ring_keeps_the_newest_records() {
        let log = Arc::new(StreamSessionLog::with_capacity(2));
        for _ in 0..3 {
            StreamSessionTracker::start(Arc::clone(&log), Uuid::nil(), "ts", None)
                .fail("no provider");
        }
        let recent = log.recent();
        assert_eq!(recent.len(), 2);
        assert!(recent[0].ended_at >= recent[1].ended_at);
    }

    #[tokio::test]
    async fn a_tracked_body_counts_bytes_and_records_the_viewer_error() {
        let log = Arc::new(StreamSessionLog::default());
        let channel_id = Uuid::new_v4();
        let tracker = StreamSessionTracker::start(Arc::clone(&log), channel_id, "mkv", None);
        let chunks: Vec<Result<Bytes, ViewerStreamError>> = vec![
            Ok(Bytes::from_static(&[0; 188])),
            Ok(Bytes::from_static(&[0; 376])),
            Err(ViewerStreamError::Upstream {
                message: "reset by peer".into(),
            }),
        ];
        let tapped = tracker.tap_viewer(stream::iter(chunks));
        let mut body = tracker.body(tapped);
        while body.next().await.is_some() {}
        drop(body);
        let record = log.recent().pop().expect("one record");
        assert_eq!(record.channel_id, channel_id);
        assert_eq!(record.container, "mkv");
        assert_eq!(record.bytes_sent, 564);
        assert_eq!(record.end_reason, StreamSessionEndReason::UpstreamError);
        assert_eq!(
            record.detail.as_deref(),
            Some("upstream stream failed: reset by peer")
        );
    }

    #[tokio::test]
    async fn a_body_dropped_early_is_a_client_disconnect() {
        let log = Arc::new(StreamSessionLog::default());
        let tracker = StreamSessionTracker::start(Arc::clone(&log), Uuid::nil(), "ts", None);
        let chunks: Vec<Result<Bytes, std::io::Error>> = vec![
            Ok(Bytes::from_static(&[0; 188])),
            Ok(Bytes::from_static(&[0; 188])),
        ];
        let mut body = tracker.body(stream::iter(chunks));
        let _ = body.next().await;
        drop(body);
        let record = log.recent().pop().expect("one record");
        assert_eq!(record.end_reason, StreamSessionEndReason::ClientDisconnect);
        assert_eq!(record.bytes_sent, 188);
        assert_eq!(record.detail, None);
    }

    #[test]
    fn a_record_serializes_without_secrets() {
        let log = Arc::new(StreamSessionLog::default());
        StreamSessionTracker::start(Arc::clone(&log), Uuid::nil(), "ts", None)
            .fail("open http://provider.example/live?username=a&password=b failed");
        let value = serde_json::to_value(log.recent()).expect("records serialize");
        let text = value.to_string();
        assert!(!text.contains("provider.example"), "{text}");
        assert!(!text.contains("password=b"), "{text}");
        assert_eq!(value[0]["endReason"], "start_failed");
        assert_eq!(value[0]["container"], "ts");
    }
}
