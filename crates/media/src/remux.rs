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
    io,
    pin::Pin,
    process::Stdio,
};

use bytes::{Bytes, BytesMut};
use futures_util::{Stream, StreamExt};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    process::Command,
};
use tracing::{debug, warn};

const REMUX_READ_BUFFER_BYTES: usize = 64 * 1024;
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
    tokio::spawn(async move {
        let mut input = Box::pin(input);
        while let Some(chunk) = input.next().await {
            let Ok(chunk) = chunk else {
                break;
            };
            if stdin.write_all(&chunk).await.is_err() {
                break;
            }
        }
        debug!("matroska remux input ended");
    });
    let output = async_stream::stream! {
        // The stream owns the child. A drop of the stream stops the process.
        let _child = child;
        let mut buffer = BytesMut::with_capacity(REMUX_READ_BUFFER_BYTES);
        loop {
            buffer.reserve(REMUX_READ_BUFFER_BYTES);
            match stdout.read_buf(&mut buffer).await {
                Ok(0) => break,
                Ok(_) => yield Ok(buffer.split().freeze()),
                Err(error) => {
                    yield Err(error);
                    break;
                }
            }
        }
    };
    Ok(Box::pin(output))
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
