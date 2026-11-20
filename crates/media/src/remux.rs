//! Live Matroska remux of a viewer stream.
//!
//! An MPEG-TS stream does not give a video bitrate to a client probe. Jellyfin
//! then estimates a bitrate from the frame width, and the estimate is often
//! much too high. A Matroska stream can have a `BPS` tag on each track, and
//! the Jellyfin probe reads that tag.
//!
//! The remux uses a fixed `FFmpeg` command:
//!
//! - The command reads MPEG-TS from its standard input.
//! - The command copies the video.
//! - The command encodes the audio again to AAC. A live Matroska pipe must
//!   write the AAC configuration in the header, and a stream copy of ADTS
//!   audio cannot supply it in time.
//! - The command writes live Matroska with `BPS` tags to its standard output.
//!
//! All argument values come from range-checked numbers. No text from a provider
//! or a client enters the arguments.

use std::{
    ffi::{OsStr, OsString},
    fmt, io,
    pin::Pin,
    process::{ExitStatus, Stdio},
    sync::{Arc, Mutex, PoisonError},
    time::Duration,
};

use bytes::{Bytes, BytesMut};
use futures_util::{Stream, StreamExt};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    process::Command,
};
use tracing::{debug, info, warn};

const REMUX_READ_BUFFER_BYTES: usize = 64 * 1024;
/// The longest time that the output waits for the `FFmpeg` exit status.
const REMUX_EXIT_WAIT: Duration = Duration::from_secs(5);
/// The largest number of `FFmpeg` error lines that one remux logs.
const MAX_LOGGED_ERROR_LINES: usize = 8;
/// The AAC bitrate of a mono or stereo track, in bits per second.
pub const STEREO_AUDIO_BITS_PER_SECOND: u64 = 128_000;
/// The AAC bitrate of a surround track, in bits per second.
pub const SURROUND_AUDIO_BITS_PER_SECOND: u64 = 384_000;
/// The part of the measured MPEG-TS rate that is not elementary stream data,
/// in percent.
const TRANSPORT_OVERHEAD_PERCENT: u64 = 5;
/// The lowest video bitrate that the remux writes in a tag.
const MIN_VIDEO_BITS_PER_SECOND: u64 = 100_000;

/// A byte stream of live Matroska.
pub type MatroskaByteStream = Pin<Box<dyn Stream<Item = Result<Bytes, io::Error>> + Send>>;

/// The bitrate tags of one Matroska remux.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MatroskaBitrates {
    /// The video `BPS` tag. `None` writes no video tag.
    pub video_bits_per_second: Option<u64>,
    /// The AAC encoder bitrate and the audio `BPS` tag.
    pub audio_bits_per_second: u64,
}

impl MatroskaBitrates {
    /// Calculates the tags from the measured MPEG-TS rate of the stream.
    ///
    /// The video value is the measured rate minus the transport overhead and
    /// minus the audio rate. A missing or zero measurement gives no video tag.
    #[must_use]
    pub fn from_measured(measured_bits_per_second: Option<u64>, audio_channels: u8) -> Self {
        let audio_bits_per_second = if audio_channels > 2 {
            SURROUND_AUDIO_BITS_PER_SECOND
        } else {
            STEREO_AUDIO_BITS_PER_SECOND
        };
        let video_bits_per_second =
            measured_bits_per_second
                .filter(|value| *value > 0)
                .map(|total| {
                    let payload = total - total * TRANSPORT_OVERHEAD_PERCENT / 100;
                    payload
                        .saturating_sub(audio_bits_per_second)
                        .max(MIN_VIDEO_BITS_PER_SECOND)
                });
        Self {
            video_bits_per_second,
            audio_bits_per_second,
        }
    }
}

/// Returns the `FFmpeg` executable of the remux.
#[must_use]
pub fn matroska_remux_executable() -> &'static OsStr {
    OsStr::new("ffmpeg")
}

/// Builds the fixed `FFmpeg` arguments of one remux.
#[must_use]
pub fn matroska_remux_arguments(bitrates: MatroskaBitrates) -> Vec<OsString> {
    let mut arguments: Vec<String> = [
        "-hide_banner",
        "-loglevel",
        "error",
        "-nostdin",
        "-fflags",
        "+genpts+discardcorrupt",
        "-f",
        "mpegts",
        "-i",
        "pipe:0",
        "-map",
        "0:v:0?",
        "-map",
        "0:a:0?",
        "-c:v",
        "copy",
        "-c:a",
        "aac",
        "-b:a",
    ]
    .map(String::from)
    .into();
    arguments.push(bitrates.audio_bits_per_second.to_string());
    if let Some(video) = bitrates.video_bits_per_second {
        arguments.push("-metadata:s:v:0".into());
        arguments.push(format!("BPS={video}"));
    }
    arguments.push("-metadata:s:a:0".into());
    arguments.push(format!("BPS={}", bitrates.audio_bits_per_second));
    arguments.extend(
        [
            "-live",
            "1",
            "-flush_packets",
            "1",
            "-f",
            "matroska",
            "pipe:1",
        ]
        .map(String::from),
    );
    arguments.into_iter().map(OsString::from).collect()
}

/// A side of the remux.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RemuxSide {
    /// The MPEG-TS input that the relay writes to `FFmpeg`.
    Input,
    /// The Matroska output that `FFmpeg` writes to the client.
    Output,
}

impl RemuxSide {
    /// Returns the name of the side.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Input => "input",
            Self::Output => "output",
        }
    }
}

/// How the MPEG-TS input of the remux ended.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RemuxInputEnd {
    /// The input stream ended without an error.
    Finished,
    /// The input stream gave an error. The value is the error message.
    Failed(String),
    /// `FFmpeg` closed its standard input.
    OutputClosed,
}

/// How the Matroska output of the remux ended.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RemuxOutputEnd {
    /// `FFmpeg` closed its standard output.
    Finished,
    /// A read from `FFmpeg` failed. The value is the error message.
    Failed(String),
}

/// The exit status of the `FFmpeg` remux process.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RemuxExit {
    /// The process exited with this code.
    Code(i32),
    /// A signal stopped the process.
    Signal(i32),
    /// The exit status is not known.
    Unknown,
}

impl RemuxExit {
    fn from_status(status: ExitStatus) -> Self {
        if let Some(code) = status.code() {
            return Self::Code(code);
        }
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;
            if let Some(signal) = status.signal() {
                return Self::Signal(signal);
            }
        }
        Self::Unknown
    }
}

impl fmt::Display for RemuxExit {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Code(code) => write!(formatter, "exit code {code}"),
            Self::Signal(signal) => write!(formatter, "signal {signal}"),
            Self::Unknown => formatter.write_str("unknown exit status"),
        }
    }
}

/// What the remux knows about how it ended.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RemuxSummary {
    /// How the input ended, if it ended.
    pub input_end: Option<RemuxInputEnd>,
    /// How the output ended, if it ended.
    pub output_end: Option<RemuxOutputEnd>,
    /// The `FFmpeg` exit status, if the output read it.
    pub exit: Option<RemuxExit>,
    /// The side that ended first.
    pub first_end: Option<RemuxSide>,
}

/// A shared report of how a remux ended.
///
/// The input task and the output stream write to the report. A caller reads
/// the report after the output ends or drops.
#[derive(Clone, Debug, Default)]
pub struct RemuxReport {
    state: Arc<Mutex<RemuxSummary>>,
}

impl RemuxReport {
    /// Returns a copy of the current summary.
    #[must_use]
    pub fn summary(&self) -> RemuxSummary {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn update(&self, change: impl FnOnce(&mut RemuxSummary)) -> RemuxSummary {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        change(&mut state);
        state.clone()
    }

    fn record_input(&self, end: RemuxInputEnd) -> RemuxSummary {
        self.update(|state| {
            state.first_end.get_or_insert(RemuxSide::Input);
            state.input_end.get_or_insert(end);
        })
    }

    fn record_output(&self, end: RemuxOutputEnd) -> RemuxSummary {
        self.update(|state| {
            state.first_end.get_or_insert(RemuxSide::Output);
            state.output_end.get_or_insert(end);
        })
    }

    fn record_exit(&self, exit: RemuxExit) -> RemuxSummary {
        self.update(|state| state.exit = Some(exit))
    }
}

/// Starts a remux of an MPEG-TS stream to live Matroska.
///
/// The returned stream owns the `FFmpeg` process. When the stream drops, the
/// process stops. Then the input task gets a write error, stops, and drops the
/// input stream.
///
/// # Errors
///
/// Returns the spawn error when `FFmpeg` cannot start.
pub fn spawn_matroska_remux<S>(
    input: S,
    bitrates: MatroskaBitrates,
) -> Result<MatroskaByteStream, io::Error>
where
    S: Stream<Item = Result<Bytes, io::Error>> + Send + 'static,
{
    spawn_matroska_remux_with_report(input, bitrates).map(|(output, _)| output)
}

/// Starts a remux of an MPEG-TS stream to live Matroska and returns a report
/// of how the remux ends.
///
/// The remux logs an input error at the warn level. When the output ends, the
/// remux logs the `FFmpeg` exit status and the side that ended first.
///
/// # Errors
///
/// Returns the spawn error when `FFmpeg` cannot start.
pub fn spawn_matroska_remux_with_report<S>(
    input: S,
    bitrates: MatroskaBitrates,
) -> Result<(MatroskaByteStream, RemuxReport), io::Error>
where
    S: Stream<Item = Result<Bytes, io::Error>> + Send + 'static,
{
    let report = RemuxReport::default();
    let mut child = Command::new(matroska_remux_executable())
        .args(matroska_remux_arguments(bitrates))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| io::Error::other("the remux process has no standard input"))?;
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("the remux process has no standard output"))?;
    if let Some(stderr) = child.stderr.take() {
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            let mut logged = 0_usize;
            while let Ok(Some(line)) = lines.next_line().await {
                if logged < MAX_LOGGED_ERROR_LINES {
                    warn!(message = %line, "matroska remux reported an error");
                    logged += 1;
                }
            }
        });
    }
    let input_report = report.clone();
    tokio::spawn(async move {
        let mut input = Box::pin(input);
        let end = loop {
            match input.next().await {
                Some(Ok(chunk)) => {
                    if stdin.write_all(&chunk).await.is_err() {
                        break RemuxInputEnd::OutputClosed;
                    }
                }
                Some(Err(error)) => break RemuxInputEnd::Failed(error.to_string()),
                None => break RemuxInputEnd::Finished,
            }
        };
        let summary = input_report.record_input(end.clone());
        let first_end = summary.first_end.map(RemuxSide::as_str);
        match end {
            RemuxInputEnd::Failed(error) => warn!(
                %error,
                first_end,
                "matroska remux input failed; FFmpeg finishes the output"
            ),
            RemuxInputEnd::Finished => debug!(first_end, "matroska remux input ended"),
            RemuxInputEnd::OutputClosed => {
                debug!(first_end, "matroska remux closed its standard input");
            }
        }
        // A drop of the standard input tells `FFmpeg` to finish the output.
    });
    let output_report = report.clone();
    let output = async_stream::stream! {
        // The stream owns the child. A drop of the stream stops the process.
        let mut child = child;
        let mut buffer = BytesMut::with_capacity(REMUX_READ_BUFFER_BYTES);
        let end = loop {
            buffer.reserve(REMUX_READ_BUFFER_BYTES);
            match stdout.read_buf(&mut buffer).await {
                Ok(0) => break RemuxOutputEnd::Finished,
                Ok(_) => yield Ok(buffer.split().freeze()),
                Err(error) => {
                    let message = error.to_string();
                    yield Err(error);
                    break RemuxOutputEnd::Failed(message);
                }
            }
        };
        output_report.record_output(end.clone());
        let exit = match tokio::time::timeout(REMUX_EXIT_WAIT, child.wait()).await {
            Ok(Ok(status)) => RemuxExit::from_status(status),
            Ok(Err(_)) | Err(_) => RemuxExit::Unknown,
        };
        let summary = output_report.record_exit(exit);
        let first_end = summary.first_end.map(RemuxSide::as_str);
        match end {
            RemuxOutputEnd::Finished => info!(
                exit = %exit,
                first_end,
                "matroska remux output ended"
            ),
            RemuxOutputEnd::Failed(error) => warn!(
                %error,
                exit = %exit,
                first_end,
                "matroska remux output failed"
            ),
        }
    };
    Ok((Box::pin(output), report))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(arguments: &[OsString]) -> Vec<&str> {
        arguments
            .iter()
            .map(|value| value.to_str().expect("arguments are UTF-8"))
            .collect()
    }

    #[test]
    fn report_keeps_the_side_that_ended_first() {
        let report = RemuxReport::default();
        report.record_input(RemuxInputEnd::Failed("upstream reset".to_owned()));
        report.record_output(RemuxOutputEnd::Finished);
        report.record_input(RemuxInputEnd::Finished);
        let summary = report.record_exit(RemuxExit::Code(0));
        assert_eq!(summary.first_end, Some(RemuxSide::Input));
        assert_eq!(
            summary.input_end,
            Some(RemuxInputEnd::Failed("upstream reset".to_owned()))
        );
        assert_eq!(summary.output_end, Some(RemuxOutputEnd::Finished));
        assert_eq!(summary.exit, Some(RemuxExit::Code(0)));

        let report = RemuxReport::default();
        report.record_output(RemuxOutputEnd::Failed("broken pipe".to_owned()));
        report.record_input(RemuxInputEnd::OutputClosed);
        assert_eq!(report.summary().first_end, Some(RemuxSide::Output));
    }

    #[cfg(unix)]
    #[test]
    fn exit_status_gives_a_code_or_a_signal() {
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(
            RemuxExit::from_status(ExitStatus::from_raw(1 << 8)),
            RemuxExit::Code(1)
        );
        assert_eq!(
            RemuxExit::from_status(ExitStatus::from_raw(9)),
            RemuxExit::Signal(9)
        );
        assert_eq!(RemuxExit::Code(1).to_string(), "exit code 1");
        assert_eq!(RemuxExit::Signal(9).to_string(), "signal 9");
    }

    #[test]
    fn measured_rate_gives_video_rate_without_overhead_and_audio() {
        let bitrates = MatroskaBitrates::from_measured(Some(3_700_000), 2);
        assert_eq!(bitrates.audio_bits_per_second, 128_000);
        assert_eq!(bitrates.video_bits_per_second, Some(3_387_000));
    }

    #[test]
    fn surround_audio_uses_the_surround_rate() {
        let bitrates = MatroskaBitrates::from_measured(Some(8_000_000), 6);
        assert_eq!(bitrates.audio_bits_per_second, 384_000);
        assert_eq!(bitrates.video_bits_per_second, Some(7_216_000));
    }

    #[test]
    fn missing_or_zero_measurement_gives_no_video_tag() {
        assert_eq!(
            MatroskaBitrates::from_measured(None, 2).video_bits_per_second,
            None
        );
        assert_eq!(
            MatroskaBitrates::from_measured(Some(0), 2).video_bits_per_second,
            None
        );
    }

    #[test]
    fn very_low_measurement_keeps_the_minimum_video_rate() {
        let bitrates = MatroskaBitrates::from_measured(Some(50_000), 2);
        assert_eq!(bitrates.video_bits_per_second, Some(100_000));
    }

    #[test]
    fn argument_array_is_exact_and_auditable() {
        let arguments = matroska_remux_arguments(MatroskaBitrates {
            video_bits_per_second: Some(3_387_000),
            audio_bits_per_second: 128_000,
        });
        assert_eq!(matroska_remux_executable(), OsStr::new("ffmpeg"));
        assert_eq!(
            text(&arguments),
            [
                "-hide_banner",
                "-loglevel",
                "error",
                "-nostdin",
                "-fflags",
                "+genpts+discardcorrupt",
                "-f",
                "mpegts",
                "-i",
                "pipe:0",
                "-map",
                "0:v:0?",
                "-map",
                "0:a:0?",
                "-c:v",
                "copy",
                "-c:a",
                "aac",
                "-b:a",
                "128000",
                "-metadata:s:v:0",
                "BPS=3387000",
                "-metadata:s:a:0",
                "BPS=128000",
                "-live",
                "1",
                "-flush_packets",
                "1",
                "-f",
                "matroska",
                "pipe:1",
            ]
        );
    }

    #[cfg(unix)]
    fn mpeg_ts_fixture() -> Vec<u8> {
        let output = std::process::Command::new("ffmpeg")
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=64x64:rate=10",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:sample_rate=48000",
                "-t",
                "2",
                "-c:v",
                "libx264",
                "-g",
                "10",
                "-c:a",
                "aac",
                "-f",
                "mpegts",
                "pipe:1",
            ])
            .output()
            .expect("ffmpeg makes the MPEG-TS fixture");
        assert!(output.status.success());
        output.stdout
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn remux_writes_live_matroska_with_bitrate_tags() {
        let fixture = mpeg_ts_fixture();
        let chunks: Vec<Result<Bytes, io::Error>> = fixture
            .chunks(7 * 188)
            .map(|chunk| Ok(Bytes::copy_from_slice(chunk)))
            .collect();
        let bitrates = MatroskaBitrates {
            video_bits_per_second: Some(3_387_000),
            audio_bits_per_second: 128_000,
        };
        let output = spawn_matroska_remux(futures_util::stream::iter(chunks), bitrates)
            .expect("the remux starts");
        let parts: Vec<Bytes> = output
            .map(|chunk| chunk.expect("the remux output reads"))
            .collect()
            .await;
        let matroska: Vec<u8> = parts.concat();
        assert_eq!(&matroska[..4], &[0x1a, 0x45, 0xdf, 0xa3]);

        let mut probe = std::process::Command::new("ffprobe")
            .args([
                "-v",
                "error",
                "-show_entries",
                "stream=codec_type,codec_name:stream_tags=BPS",
                "-of",
                "compact",
                "-i",
                "pipe:0",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("ffprobe starts");
        std::io::Write::write_all(
            probe.stdin.as_mut().expect("ffprobe has a standard input"),
            &matroska,
        )
        .expect("ffprobe reads the remux output");
        let report = probe.wait_with_output().expect("ffprobe ends");
        let report = String::from_utf8_lossy(&report.stdout);
        assert!(
            report.contains("codec_name=h264|codec_type=video|tag:BPS=3387000"),
            "{report}"
        );
        assert!(
            report.contains("codec_name=aac|codec_type=audio|tag:BPS=128000"),
            "{report}"
        );
    }

    #[test]
    fn no_video_measurement_omits_the_video_tag() {
        let arguments = matroska_remux_arguments(MatroskaBitrates::from_measured(None, 2));
        let arguments = text(&arguments);
        assert!(!arguments.contains(&"-metadata:s:v:0"));
        assert!(arguments.contains(&"BPS=128000"));
    }
}
