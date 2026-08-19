//! Audited `FFmpeg` and VLC input-process adapters.
//!
//! Child processes can read only from a validated loopback broker URL. Provider
//! URLs, credentials, arbitrary arguments, and executable overrides are not
//! representable through the public API.

use std::{
    ffi::{OsStr, OsString},
    fmt,
    io::ErrorKind,
    net::IpAddr,
    process::{ExitStatus, Stdio},
};

use bytes::BytesMut;
use thiserror::Error;
use tokio::{
    io::AsyncReadExt,
    process::{Child, ChildStdout, Command},
    sync::{oneshot, watch},
};

use crate::{
    MpegTsPacketizer, MpegTsRing, MpegTsRingConfig, RingCloseReason, RingCursor, RingSnapshot,
};

const PROCESS_READ_BUFFER_BYTES: usize = 32 * 1024;

/// A process input that is guaranteed to target the local broker.
#[derive(Clone, Eq, PartialEq)]
pub struct InputSource {
    broker_url: reqwest::Url,
}

impl fmt::Debug for InputSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("InputSource")
            .field("broker_url", &"http://<loopback>/<redacted>")
            .finish()
    }
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum InputSourceError {
    #[error("input source is not a valid URL")]
    InvalidUrl,
    #[error("process inputs must use plain HTTP to the loopback broker")]
    UnsupportedScheme,
    #[error("process inputs must target a loopback IP address or localhost")]
    NotLoopback,
    #[error("process input URLs cannot contain user information")]
    UserInformation,
    #[error("process input URLs cannot contain fragments")]
    Fragment,
}

impl InputSource {
    /// Creates a process input restricted to the local HTTP broker.
    ///
    /// Query parameters are permitted because the broker may use a short-lived
    /// internal token, but debug and error output never includes the URL.
    ///
    /// # Errors
    ///
    /// Returns an error for malformed, non-HTTP, non-loopback, user-info, or
    /// fragment-bearing URLs.
    pub fn loopback_http(url: &str) -> Result<Self, InputSourceError> {
        let broker_url = reqwest::Url::parse(url).map_err(|_| InputSourceError::InvalidUrl)?;
        if broker_url.scheme() != "http" {
            return Err(InputSourceError::UnsupportedScheme);
        }
        if !broker_url.username().is_empty() || broker_url.password().is_some() {
            return Err(InputSourceError::UserInformation);
        }
        if broker_url.fragment().is_some() {
            return Err(InputSourceError::Fragment);
        }

        let host = broker_url.host_str().ok_or(InputSourceError::NotLoopback)?;
        let host = host
            .strip_prefix('[')
            .and_then(|host| host.strip_suffix(']'))
            .unwrap_or(host);
        let is_loopback = host.eq_ignore_ascii_case("localhost")
            || host
                .parse::<IpAddr>()
                .is_ok_and(|address| address.is_loopback());
        if !is_loopback {
            return Err(InputSourceError::NotLoopback);
        }

        Ok(Self { broker_url })
    }

    fn command_argument(&self) -> OsString {
        self.broker_url.as_str().into()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProcessAdapterKind {
    Ffmpeg,
    Vlc,
}

impl ProcessAdapterKind {
    pub fn executable(self) -> &'static OsStr {
        match self {
            Self::Ffmpeg => OsStr::new("ffmpeg"),
            Self::Vlc => OsStr::new("vlc"),
        }
    }
}

impl fmt::Display for ProcessAdapterKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ffmpeg => formatter.write_str("FFmpeg"),
            Self::Vlc => formatter.write_str("VLC"),
        }
    }
}

/// An inspectable, non-extensible process invocation.
///
/// Its arguments are assembled only by the typed adapters. Debug output omits
/// all arguments because the broker URL may contain an internal access token.
#[derive(Clone, Eq, PartialEq)]
pub struct AuditedProcessCommand {
    adapter: ProcessAdapterKind,
    executable: &'static OsStr,
    arguments: Vec<OsString>,
}

impl fmt::Debug for AuditedProcessCommand {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AuditedProcessCommand")
            .field("adapter", &self.adapter)
            .field("executable", &self.executable)
            .field("arguments", &"<redacted audited arguments>")
            .finish()
    }
}

impl AuditedProcessCommand {
    pub fn adapter(&self) -> ProcessAdapterKind {
        self.adapter
    }

    pub fn executable(&self) -> &OsStr {
        self.executable
    }

    /// Returns the audited argument array. It can contain only a loopback URL,
    /// never a provider URL or provider credentials.
    pub fn arguments(&self) -> &[OsString] {
        &self.arguments
    }

    fn spawn(
        self,
        ring_config: MpegTsRingConfig,
    ) -> Result<ProcessInputSession, ProcessAdapterError> {
        spawn_command(self, ring_config)
    }

    #[cfg(test)]
    fn test_only(
        adapter: ProcessAdapterKind,
        executable: &'static OsStr,
        arguments: Vec<OsString>,
    ) -> Self {
        Self {
            adapter,
            executable,
            arguments,
        }
    }
}

/// Fixed `FFmpeg` remux-to-MPEG-TS adapter.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct FfmpegInputAdapter {
    _private: (),
}

impl FfmpegInputAdapter {
    pub const fn new() -> Self {
        Self { _private: () }
    }

    #[must_use]
    pub fn command(self, input: &InputSource) -> AuditedProcessCommand {
        AuditedProcessCommand {
            adapter: ProcessAdapterKind::Ffmpeg,
            executable: ProcessAdapterKind::Ffmpeg.executable(),
            arguments: vec![
                "-hide_banner".into(),
                "-loglevel".into(),
                "error".into(),
                "-nostdin".into(),
                "-i".into(),
                input.command_argument(),
                "-map".into(),
                "0".into(),
                "-c".into(),
                "copy".into(),
                "-f".into(),
                "mpegts".into(),
                "pipe:1".into(),
            ],
        }
    }

    /// Starts `FFmpeg` with stdout connected to a packet-aligned ring.
    ///
    /// # Errors
    ///
    /// Returns a redacted spawn/setup error. Runtime read, packetization, and
    /// exit failures are reported through [`ProcessExit`] and the ring close.
    pub fn start(
        self,
        input: &InputSource,
        ring_config: MpegTsRingConfig,
    ) -> Result<ProcessInputSession, ProcessAdapterError> {
        self.command(input).spawn(ring_config)
    }
}

/// Fixed VLC remux-to-MPEG-TS adapter.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct VlcInputAdapter {
    _private: (),
}

impl VlcInputAdapter {
    pub const fn new() -> Self {
        Self { _private: () }
    }

    #[must_use]
    pub fn command(self, input: &InputSource) -> AuditedProcessCommand {
        AuditedProcessCommand {
            adapter: ProcessAdapterKind::Vlc,
            executable: ProcessAdapterKind::Vlc.executable(),
            arguments: vec![
                "--intf".into(),
                "dummy".into(),
                "--no-video-title-show".into(),
                "--quiet".into(),
                input.command_argument(),
                "--sout".into(),
                "#standard{access=file,mux=ts,dst=-}".into(),
                "vlc://quit".into(),
            ],
        }
    }

    /// Starts VLC with stdout connected to a packet-aligned ring.
    ///
    /// # Errors
    ///
    /// Returns a redacted spawn/setup error. Runtime read, packetization, and
    /// exit failures are reported through [`ProcessExit`] and the ring close.
    pub fn start(
        self,
        input: &InputSource,
        ring_config: MpegTsRingConfig,
    ) -> Result<ProcessInputSession, ProcessAdapterError> {
        self.command(input).spawn(ring_config)
    }
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum ProcessAdapterError {
    #[error("could not spawn the fixed {adapter} executable ({kind:?})")]
    Spawn {
        adapter: ProcessAdapterKind,
        kind: ErrorKind,
    },
    #[error("the fixed {adapter} process did not expose its configured stdout pipe")]
    MissingStdout { adapter: ProcessAdapterKind },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProcessFailureStage {
    ReadStdout,
    Packetize,
    WriteRing,
    Kill,
    Wait,
    SupervisorLost,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProcessExit {
    Completed {
        success: bool,
        code: Option<i32>,
    },
    Cancelled {
        code: Option<i32>,
    },
    Failed {
        stage: ProcessFailureStage,
        kind: Option<ErrorKind>,
    },
}

/// A running process and its output ring.
///
/// Dropping this handle requests shutdown. [`Self::shutdown`] additionally
/// waits until the child has been killed and reaped.
pub struct ProcessInputSession {
    adapter: ProcessAdapterKind,
    process_id: Option<u32>,
    ring: MpegTsRing,
    shutdown: Option<oneshot::Sender<()>>,
    completion: watch::Receiver<Option<ProcessExit>>,
}

impl fmt::Debug for ProcessInputSession {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProcessInputSession")
            .field("adapter", &self.adapter)
            .field("process_id", &self.process_id)
            .field("ring", &self.ring.snapshot())
            .finish_non_exhaustive()
    }
}

impl Drop for ProcessInputSession {
    fn drop(&mut self) {
        self.request_shutdown();
    }
}

impl ProcessInputSession {
    pub fn adapter(&self) -> ProcessAdapterKind {
        self.adapter
    }

    pub fn process_id(&self) -> Option<u32> {
        self.process_id
    }

    pub fn subscribe(&self) -> RingCursor {
        self.ring.subscribe()
    }

    pub fn snapshot(&self) -> RingSnapshot {
        self.ring.snapshot()
    }

    /// Idempotently asks the supervisor to kill and reap the child.
    pub fn request_shutdown(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
    }

    /// Waits for natural completion without requesting cancellation.
    pub async fn wait(&mut self) -> ProcessExit {
        loop {
            if let Some(exit) = self.completion.borrow_and_update().clone() {
                return exit;
            }
            if self.completion.changed().await.is_err() {
                return ProcessExit::Failed {
                    stage: ProcessFailureStage::SupervisorLost,
                    kind: None,
                };
            }
        }
    }

    /// Requests cancellation and waits until the child has been killed/reaped.
    pub async fn shutdown(mut self) -> ProcessExit {
        self.request_shutdown();
        self.wait().await
    }
}

fn spawn_command(
    plan: AuditedProcessCommand,
    ring_config: MpegTsRingConfig,
) -> Result<ProcessInputSession, ProcessAdapterError> {
    let adapter = plan.adapter;
    let mut command = Command::new(plan.executable);
    command
        .args(plan.arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        // Prevent inherited proxy settings from routing loopback broker traffic
        // through an external service.
        .env_remove("HTTP_PROXY")
        .env_remove("http_proxy")
        .env_remove("HTTPS_PROXY")
        .env_remove("https_proxy")
        .env_remove("ALL_PROXY")
        .env_remove("all_proxy")
        .env("NO_PROXY", "127.0.0.1,::1,localhost")
        .env("no_proxy", "127.0.0.1,::1,localhost");

    let mut child = command
        .spawn()
        .map_err(|error| ProcessAdapterError::Spawn {
            adapter,
            kind: error.kind(),
        })?;
    let process_id = child.id();
    let stdout = child
        .stdout
        .take()
        .ok_or(ProcessAdapterError::MissingStdout { adapter })?;
    let ring = MpegTsRing::new(ring_config);
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let (completion_tx, completion_rx) = watch::channel(None);
    tokio::spawn(supervise_process(
        adapter,
        child,
        stdout,
        ring.clone(),
        shutdown_rx,
        completion_tx,
    ));

    Ok(ProcessInputSession {
        adapter,
        process_id,
        ring,
        shutdown: Some(shutdown_tx),
        completion: completion_rx,
    })
}

async fn supervise_process(
    adapter: ProcessAdapterKind,
    mut child: Child,
    mut stdout: ChildStdout,
    ring: MpegTsRing,
    mut shutdown: oneshot::Receiver<()>,
    completion: watch::Sender<Option<ProcessExit>>,
) {
    let mut packetizer = MpegTsPacketizer::new();
    let mut buffer = BytesMut::zeroed(PROCESS_READ_BUFFER_BYTES);

    let exit = loop {
        tokio::select! {
            biased;
            _ = &mut shutdown => {
                ring.close(RingCloseReason::Shutdown);
                break terminate_and_reap(&mut child).await;
            }
            read = stdout.read(&mut buffer) => {
                match read {
                    Ok(0) => {
                        break finish_naturally(adapter, &mut child, &ring, packetizer).await;
                    }
                    Ok(count) => {
                        match packetizer.push(&buffer[..count]) {
                            Ok(packets) => {
                                if !packets.is_empty()
                                    && ring.push(&packets).is_err()
                                {
                                    break fail_and_reap(
                                        &mut child,
                                        &ring,
                                        ProcessFailureStage::WriteRing,
                                        None,
                                    ).await;
                                }
                            }
                            Err(_) => {
                                break fail_and_reap(
                                    &mut child,
                                    &ring,
                                    ProcessFailureStage::Packetize,
                                    None,
                                ).await;
                            }
                        }
                    }
                    Err(error) => {
                        break fail_and_reap(
                            &mut child,
                            &ring,
                            ProcessFailureStage::ReadStdout,
                            Some(error.kind()),
                        ).await;
                    }
                }
            }
        }
    };

    let _ = completion.send(Some(exit));
}

async fn finish_naturally(
    adapter: ProcessAdapterKind,
    child: &mut Child,
    ring: &MpegTsRing,
    packetizer: MpegTsPacketizer,
) -> ProcessExit {
    if packetizer.finish().is_err() {
        return fail_and_reap(child, ring, ProcessFailureStage::Packetize, None).await;
    }

    match child.wait().await {
        Ok(status) => {
            if status.success() {
                ring.close(RingCloseReason::EndOfStream);
            } else {
                ring.close(redacted_process_failure(adapter, "exited unsuccessfully"));
            }
            completed_exit(status)
        }
        Err(error) => {
            ring.close(redacted_process_failure(adapter, "could not be reaped"));
            ProcessExit::Failed {
                stage: ProcessFailureStage::Wait,
                kind: Some(error.kind()),
            }
        }
    }
}

async fn terminate_and_reap(child: &mut Child) -> ProcessExit {
    if let Err(error) = child.start_kill()
        && child.try_wait().ok().flatten().is_none()
    {
        return ProcessExit::Failed {
            stage: ProcessFailureStage::Kill,
            kind: Some(error.kind()),
        };
    }

    match child.wait().await {
        Ok(status) => ProcessExit::Cancelled {
            code: status.code(),
        },
        Err(error) => ProcessExit::Failed {
            stage: ProcessFailureStage::Wait,
            kind: Some(error.kind()),
        },
    }
}

async fn fail_and_reap(
    child: &mut Child,
    ring: &MpegTsRing,
    stage: ProcessFailureStage,
    kind: Option<ErrorKind>,
) -> ProcessExit {
    ring.close(RingCloseReason::UpstreamError(
        "process input failed; arguments redacted".into(),
    ));
    let _ = child.start_kill();
    if let Err(error) = child.wait().await {
        return ProcessExit::Failed {
            stage: ProcessFailureStage::Wait,
            kind: Some(error.kind()),
        };
    }
    ProcessExit::Failed { stage, kind }
}

fn completed_exit(status: ExitStatus) -> ProcessExit {
    ProcessExit::Completed {
        success: status.success(),
        code: status.code(),
    }
}

fn redacted_process_failure(adapter: ProcessAdapterKind, detail: &'static str) -> RingCloseReason {
    RingCloseReason::UpstreamError(format!("{adapter} {detail}; arguments redacted").into())
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::{Path, PathBuf},
        time::Duration,
    };

    use tokio::time::timeout;
    use uuid::Uuid;

    use crate::{
        MPEG_TS_PACKET_SIZE, RingRead,
        psi::test_support::{TEST_VIDEO_PID, pat_packet, pmt_packet},
    };

    use super::*;

    fn argument_strings(command: &AuditedProcessCommand) -> Vec<String> {
        command
            .arguments()
            .iter()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect()
    }

    fn packet(id: u32) -> [u8; MPEG_TS_PACKET_SIZE] {
        let mut packet = [0xff; MPEG_TS_PACKET_SIZE];
        packet[0] = 0x47;
        packet[1] = u8::try_from((TEST_VIDEO_PID >> 8) & 0x1f).unwrap();
        packet[2] = u8::try_from(TEST_VIDEO_PID & 0xff).unwrap();
        packet[3] = 0x10;
        packet[4..8].copy_from_slice(&id.to_be_bytes());
        packet
    }

    fn packets(ids: impl IntoIterator<Item = u32>) -> Vec<u8> {
        [
            pat_packet().to_vec(),
            pmt_packet().to_vec(),
            ids.into_iter().flat_map(packet).collect(),
        ]
        .concat()
    }

    fn temp_fixture(contents: &[u8]) -> PathBuf {
        let path = std::env::temp_dir().join(format!("iptv-media-{}.ts", Uuid::new_v4()));
        fs::write(&path, contents).unwrap();
        path
    }

    fn remove_fixture(path: &Path) {
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn input_source_accepts_only_redacted_loopback_http_urls() {
        for valid in [
            "http://127.0.0.1:8080/play/channel?token=internal-secret",
            "http://[::1]:8080/play/channel",
            "http://localhost:8080/play/channel",
        ] {
            let source = InputSource::loopback_http(valid).unwrap();
            let debug = format!("{source:?}");
            assert!(!debug.contains("internal-secret"));
            assert!(!debug.contains("channel"));
        }

        assert_eq!(
            InputSource::loopback_http("https://127.0.0.1/live"),
            Err(InputSourceError::UnsupportedScheme)
        );
        assert_eq!(
            InputSource::loopback_http("http://example.com/live"),
            Err(InputSourceError::NotLoopback)
        );
        assert_eq!(
            InputSource::loopback_http("http://user:secret@127.0.0.1/live"),
            Err(InputSourceError::UserInformation)
        );
        assert_eq!(
            InputSource::loopback_http("http://127.0.0.1/live#secret"),
            Err(InputSourceError::Fragment)
        );
    }

    #[test]
    fn ffmpeg_argument_array_is_exact_and_auditable() {
        let source = InputSource::loopback_http("http://127.0.0.1:8080/play/1?token=x").unwrap();
        let command = FfmpegInputAdapter::new().command(&source);

        assert_eq!(command.executable(), OsStr::new("ffmpeg"));
        assert_eq!(
            argument_strings(&command),
            [
                "-hide_banner",
                "-loglevel",
                "error",
                "-nostdin",
                "-i",
                "http://127.0.0.1:8080/play/1?token=x",
                "-map",
                "0",
                "-c",
                "copy",
                "-f",
                "mpegts",
                "pipe:1",
            ]
        );
        let debug = format!("{command:?}");
        assert!(!debug.contains("token=x"));
    }

    #[test]
    fn vlc_argument_array_is_exact_and_auditable() {
        let source = InputSource::loopback_http("http://[::1]:8080/play/1?token=x").unwrap();
        let command = VlcInputAdapter::new().command(&source);

        assert_eq!(command.executable(), OsStr::new("vlc"));
        assert_eq!(
            argument_strings(&command),
            [
                "--intf",
                "dummy",
                "--no-video-title-show",
                "--quiet",
                "http://[::1]:8080/play/1?token=x",
                "--sout",
                "#standard{access=file,mux=ts,dst=-}",
                "vlc://quit",
            ]
        );
        let debug = format!("{command:?}");
        assert!(!debug.contains("token=x"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn harmless_cat_helper_feeds_aligned_stdout_and_is_reaped() {
        let expected = packets(0..5);
        let fixture = temp_fixture(&expected);
        let plan = AuditedProcessCommand::test_only(
            ProcessAdapterKind::Ffmpeg,
            OsStr::new("/bin/cat"),
            vec![fixture.as_os_str().to_owned()],
        );
        let mut session = spawn_command(plan, MpegTsRingConfig::new(8, 8).unwrap()).unwrap();
        let mut cursor = session.subscribe();
        let mut actual = Vec::new();

        loop {
            match timeout(Duration::from_secs(2), cursor.next(8))
                .await
                .unwrap()
                .unwrap()
            {
                RingRead::Packets { bytes, .. } => actual.extend_from_slice(&bytes),
                RingRead::Closed {
                    reason: RingCloseReason::EndOfStream,
                    ..
                } => break,
                event => panic!("unexpected ring event: {event:?}"),
            }
        }

        assert_eq!(actual, expected);
        assert_eq!(
            session.wait().await,
            ProcessExit::Completed {
                success: true,
                code: Some(0)
            }
        );
        remove_fixture(&fixture);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn process_output_without_its_referenced_pmt_is_never_exposed() {
        let fixture = temp_fixture(&pat_packet());
        let plan = AuditedProcessCommand::test_only(
            ProcessAdapterKind::Ffmpeg,
            OsStr::new("/bin/cat"),
            vec![fixture.as_os_str().to_owned()],
        );
        let mut session = spawn_command(plan, MpegTsRingConfig::new(8, 8).unwrap()).unwrap();
        let mut cursor = session.subscribe();
        assert_eq!(
            session.wait().await,
            ProcessExit::Completed {
                success: true,
                code: Some(0)
            }
        );
        assert_eq!(
            cursor.next(8).await.unwrap(),
            RingRead::Closed {
                generation: 0,
                reason: RingCloseReason::EndOfStream,
            }
        );
        remove_fixture(&fixture);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn explicit_shutdown_kills_and_reaps_without_installed_media_tools() {
        let plan = AuditedProcessCommand::test_only(
            ProcessAdapterKind::Vlc,
            OsStr::new("/bin/sleep"),
            vec!["30".into()],
        );
        let session = spawn_command(plan, MpegTsRingConfig::new(8, 0).unwrap()).unwrap();
        assert!(session.process_id().is_some());

        let exit = timeout(Duration::from_secs(2), session.shutdown())
            .await
            .expect("shutdown must kill and reap promptly");
        assert!(matches!(exit, ProcessExit::Cancelled { .. }));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn dropping_session_requests_shutdown_and_closes_waiting_readers() {
        let plan = AuditedProcessCommand::test_only(
            ProcessAdapterKind::Ffmpeg,
            OsStr::new("/bin/sleep"),
            vec!["30".into()],
        );
        let session = spawn_command(plan, MpegTsRingConfig::new(8, 0).unwrap()).unwrap();
        let mut cursor = session.subscribe();
        drop(session);

        assert!(matches!(
            timeout(Duration::from_secs(2), cursor.next(8))
                .await
                .expect("drop must promptly signal the supervisor")
                .unwrap(),
            RingRead::Closed {
                reason: RingCloseReason::Shutdown,
                ..
            }
        ));
    }

    #[cfg(unix)]
    #[test]
    fn spawn_errors_never_disclose_executable_or_arguments() {
        let plan = AuditedProcessCommand::test_only(
            ProcessAdapterKind::Ffmpeg,
            OsStr::new("/definitely-missing-provider-secret"),
            vec!["provider-password".into()],
        );
        let error = spawn_command(plan, MpegTsRingConfig::new(8, 0).unwrap()).unwrap_err();
        let debug = format!("{error:?}");
        let display = error.to_string();

        assert!(!debug.contains("provider-secret"));
        assert!(!debug.contains("provider-password"));
        assert!(!display.contains("provider-secret"));
        assert!(!display.contains("provider-password"));
        assert!(matches!(error, ProcessAdapterError::Spawn { .. }));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn invalid_helper_output_is_redacted_and_process_is_reaped() {
        let fixture = temp_fixture(&[0; MPEG_TS_PACKET_SIZE]);
        let plan = AuditedProcessCommand::test_only(
            ProcessAdapterKind::Ffmpeg,
            OsStr::new("/bin/cat"),
            vec![fixture.as_os_str().to_owned()],
        );
        let mut session = spawn_command(plan, MpegTsRingConfig::new(8, 0).unwrap()).unwrap();

        assert_eq!(
            session.wait().await,
            ProcessExit::Failed {
                stage: ProcessFailureStage::Packetize,
                kind: None
            }
        );
        let debug = format!("{session:?}");
        assert!(!debug.contains(fixture.to_string_lossy().as_ref()));
        assert!(matches!(
            session.snapshot().closed,
            Some(RingCloseReason::UpstreamError(_))
        ));
        remove_fixture(&fixture);
    }
}
