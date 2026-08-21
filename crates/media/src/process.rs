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
    CredentialBroker, CredentialBrokerEndpoint, CredentialBrokerError, HlsBrokerConfig,
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

    #[cfg(test)]
    pub(crate) fn broker_url(&self) -> &reqwest::Url {
        &self.broker_url
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProcessAdapterKind {
    Ffmpeg,
    Vlc,
}

/// The explicit media format that a fixed process reads from its broker.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum BrokeredInputFormat {
    #[default]
    DirectMpegTs,
    Hls(HlsBrokerConfig),
}

impl ProcessAdapterKind {
    pub fn executable(self) -> &'static OsStr {
        match self {
            Self::Ffmpeg => OsStr::new("ffmpeg"),
            Self::Vlc => OsStr::new("vlc"),
        }
    }

    /// Start this adapter through a loopback credential broker.
    ///
    /// The child receives only the broker URL. The broker keeps provider URLs
    /// and header values out of child arguments and debug output.
    ///
    /// This method supports one direct media response. It does not support HLS
    /// manifests or HLS segment requests.
    ///
    /// # Errors
    ///
    /// Returns a redacted error if the broker or fixed process cannot start.
    pub async fn start_brokered(
        self,
        client: reqwest::Client,
        endpoint: CredentialBrokerEndpoint,
        ring_config: MpegTsRingConfig,
    ) -> Result<BrokeredProcessInputSession, BrokeredProcessStartError> {
        self.start_brokered_with_format(
            client,
            endpoint,
            BrokeredInputFormat::DirectMpegTs,
            ring_config,
        )
        .await
    }

    /// Start this adapter with an explicit broker input format.
    ///
    /// This method does not infer HLS from an endpoint URL or credentials.
    ///
    /// # Errors
    ///
    /// Returns a redacted error if the broker or fixed process cannot start.
    pub async fn start_brokered_with_format(
        self,
        client: reqwest::Client,
        endpoint: CredentialBrokerEndpoint,
        input_format: BrokeredInputFormat,
        ring_config: MpegTsRingConfig,
    ) -> Result<BrokeredProcessInputSession, BrokeredProcessStartError> {
        start_brokered(
            client,
            endpoint,
            input_format,
            ring_config,
            |input, ring_config| match self {
                Self::Ffmpeg => FfmpegInputAdapter::new().start(input, ring_config),
                Self::Vlc => VlcInputAdapter::new().start(input, ring_config),
            },
        )
        .await
    }

    /// Start this adapter and retain a provider slot through cleanup.
    ///
    /// The returned session releases `lease` only after the child process and
    /// loopback credential broker stop.
    ///
    /// # Errors
    ///
    /// Returns a redacted error if the broker or fixed process cannot start.
    pub async fn start_brokered_leased(
        self,
        client: reqwest::Client,
        endpoint: CredentialBrokerEndpoint,
        ring_config: MpegTsRingConfig,
        lease: crate::SlotLease,
    ) -> Result<LeasedBrokeredProcessInputSession, BrokeredProcessStartError> {
        self.start_brokered_leased_with_format(
            client,
            endpoint,
            BrokeredInputFormat::DirectMpegTs,
            ring_config,
            lease,
        )
        .await
    }

    /// Start this adapter with a lease and explicit broker input format.
    ///
    /// The returned session retains `lease` until the child and broker stop.
    ///
    /// # Errors
    ///
    /// Returns a redacted error if the broker or fixed process cannot start.
    pub async fn start_brokered_leased_with_format(
        self,
        client: reqwest::Client,
        endpoint: CredentialBrokerEndpoint,
        input_format: BrokeredInputFormat,
        ring_config: MpegTsRingConfig,
        lease: crate::SlotLease,
    ) -> Result<LeasedBrokeredProcessInputSession, BrokeredProcessStartError> {
        Ok(self
            .start_brokered_with_format(client, endpoint, input_format, ring_config)
            .await?
            .into_leased(lease))
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
                "#standard{access=fd,mux=ts,dst=1}".into(),
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

/// A broker or fixed process could not start.
#[derive(Debug, Error)]
pub enum BrokeredProcessStartError {
    #[error("could not start the loopback credential broker")]
    Broker(#[from] CredentialBrokerError),
    #[error("could not start the fixed process input")]
    Process(#[from] ProcessAdapterError),
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

/// A process session that owns its loopback credential broker.
///
/// Drop this session to request process and broker shutdown.
pub struct BrokeredProcessInputSession {
    process: ProcessInputSession,
    broker: Option<CredentialBroker>,
}

/// A brokered process session that owns a provider slot through cleanup.
///
/// The cleanup task owns the [`SlotLease`]. It drops the lease only after it
/// reaps the child process and stops the credential broker.
pub struct LeasedBrokeredProcessInputSession {
    adapter: ProcessAdapterKind,
    process_id: Option<u32>,
    ring: MpegTsRing,
    shutdown: Option<oneshot::Sender<()>>,
    completion: watch::Receiver<Option<Result<ProcessExit, CredentialBrokerError>>>,
}

impl fmt::Debug for BrokeredProcessInputSession {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BrokeredProcessInputSession")
            .field("process", &self.process)
            .field("broker_active", &self.broker.is_some())
            .finish_non_exhaustive()
    }
}

impl Drop for BrokeredProcessInputSession {
    fn drop(&mut self) {
        self.request_shutdown();
    }
}

impl BrokeredProcessInputSession {
    pub fn adapter(&self) -> ProcessAdapterKind {
        self.process.adapter()
    }

    pub fn process_id(&self) -> Option<u32> {
        self.process.process_id()
    }

    pub fn subscribe(&self) -> RingCursor {
        self.process.subscribe()
    }

    pub fn snapshot(&self) -> RingSnapshot {
        self.process.snapshot()
    }

    /// Transfer this session and a provider slot to a cleanup task.
    ///
    /// Use the returned handle for a process-backed shared media session. The
    /// cleanup task retains the slot until the child and broker both stop.
    #[must_use]
    pub fn into_leased(self, lease: crate::SlotLease) -> LeasedBrokeredProcessInputSession {
        LeasedBrokeredProcessInputSession::new(self, lease)
    }

    /// Idempotently request process and broker shutdown.
    pub fn request_shutdown(&mut self) {
        self.process.request_shutdown();
        if let Some(broker) = &mut self.broker {
            broker.request_shutdown();
        }
    }

    /// Wait for process completion and broker cleanup.
    ///
    /// # Errors
    ///
    /// Returns an error if the broker task stops unexpectedly.
    pub async fn wait(&mut self) -> Result<ProcessExit, CredentialBrokerError> {
        let exit = self.process.wait().await;
        self.shutdown_broker().await?;
        Ok(exit)
    }

    /// Request shutdown and wait for process and broker cleanup.
    ///
    /// # Errors
    ///
    /// Returns an error if the broker task stops unexpectedly.
    pub async fn shutdown(mut self) -> Result<ProcessExit, CredentialBrokerError> {
        self.request_shutdown();
        self.wait().await
    }

    async fn shutdown_broker(&mut self) -> Result<(), CredentialBrokerError> {
        let Some(broker) = self.broker.take() else {
            return Ok(());
        };
        broker.shutdown().await
    }
}

impl fmt::Debug for LeasedBrokeredProcessInputSession {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LeasedBrokeredProcessInputSession")
            .field("adapter", &self.adapter)
            .field("process_id", &self.process_id)
            .field("ring", &self.ring.snapshot())
            .finish_non_exhaustive()
    }
}

impl Drop for LeasedBrokeredProcessInputSession {
    fn drop(&mut self) {
        self.request_shutdown();
    }
}

impl LeasedBrokeredProcessInputSession {
    fn new(session: BrokeredProcessInputSession, lease: crate::SlotLease) -> Self {
        let adapter = session.adapter();
        let process_id = session.process_id();
        let ring = session.process.ring.clone();
        let (shutdown_tx, mut shutdown_rx) = oneshot::channel();
        let (completion_tx, completion) = watch::channel(None);

        tokio::spawn(async move {
            let mut session = session;
            let result = tokio::select! {
                _ = &mut shutdown_rx => {
                    session.request_shutdown();
                    session.wait().await
                },
                result = session.wait() => result,
            };
            let _ = completion_tx.send(Some(result));
            drop(lease);
        });

        Self {
            adapter,
            process_id,
            ring,
            shutdown: Some(shutdown_tx),
            completion,
        }
    }

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

    pub(crate) fn ring(&self) -> MpegTsRing {
        self.ring.clone()
    }

    /// Idempotently request process and broker shutdown.
    pub fn request_shutdown(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
    }

    /// Wait until the child process and credential broker stop.
    ///
    /// # Errors
    ///
    /// Returns an error if the credential broker task stops unexpectedly.
    pub async fn wait(&mut self) -> Result<ProcessExit, CredentialBrokerError> {
        loop {
            if let Some(result) = self.completion.borrow_and_update().clone() {
                return result;
            }
            if self.completion.changed().await.is_err() {
                return Err(CredentialBrokerError::TaskStopped);
            }
        }
    }

    /// Request shutdown and wait until the child and broker stop.
    ///
    /// # Errors
    ///
    /// Returns an error if the credential broker task stops unexpectedly.
    pub async fn shutdown(mut self) -> Result<ProcessExit, CredentialBrokerError> {
        self.request_shutdown();
        self.wait().await
    }
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

async fn start_brokered<F>(
    client: reqwest::Client,
    endpoint: CredentialBrokerEndpoint,
    input_format: BrokeredInputFormat,
    ring_config: MpegTsRingConfig,
    start_process: F,
) -> Result<BrokeredProcessInputSession, BrokeredProcessStartError>
where
    F: FnOnce(&InputSource, MpegTsRingConfig) -> Result<ProcessInputSession, ProcessAdapterError>,
{
    let broker = match input_format {
        BrokeredInputFormat::DirectMpegTs => CredentialBroker::start(client, endpoint).await?,
        BrokeredInputFormat::Hls(config) => {
            CredentialBroker::start_hls(client, endpoint, config).await?
        }
    };
    let process = start_process(broker.input_source(), ring_config)?;
    Ok(BrokeredProcessInputSession {
        process,
        broker: Some(broker),
    })
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
        sync::{
            Arc, Mutex,
            atomic::{AtomicUsize, Ordering},
        },
        time::Duration,
    };

    use axum::{
        Router,
        body::Body,
        extract::State,
        http::{HeaderMap, HeaderValue, StatusCode, header},
        response::Response,
        routing::get,
    };
    use bytes::Bytes;
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

    async fn read_packets_to_end(cursor: &mut RingCursor) -> Vec<u8> {
        let mut packets = Vec::new();
        loop {
            match timeout(Duration::from_secs(2), cursor.next(8))
                .await
                .unwrap()
                .unwrap()
            {
                RingRead::Packets { bytes, .. } => packets.extend_from_slice(&bytes),
                RingRead::Closed {
                    reason: RingCloseReason::EndOfStream,
                    ..
                } => return packets,
                event => panic!("unexpected ring event: {event:?}"),
            }
        }
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
                "#standard{access=fd,mux=ts,dst=1}",
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
    async fn brokered_process_receives_only_a_loopback_url_and_streams_direct_media() {
        #[derive(Clone)]
        struct UpstreamState {
            requests: Arc<AtomicUsize>,
            expected: Vec<u8>,
        }

        async fn upstream(State(state): State<UpstreamState>, headers: HeaderMap) -> Response {
            let authorized = headers
                .get(header::AUTHORIZATION)
                .is_some_and(|value| value == "Bearer provider-secret");
            if !authorized {
                let mut response = Response::new(Body::empty());
                *response.status_mut() = StatusCode::UNAUTHORIZED;
                return response;
            }
            state.requests.fetch_add(1, Ordering::SeqCst);
            Response::new(Body::from(state.expected))
        }

        let expected = packets(0..5);
        let requests = Arc::new(AtomicUsize::new(0));
        let upstream_listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let upstream_address = upstream_listener.local_addr().unwrap();
        let upstream_requests = Arc::clone(&requests);
        let upstream_expected = expected.clone();
        let upstream_task = tokio::spawn(async move {
            axum::serve(
                upstream_listener,
                Router::new()
                    .route("/provider/live", get(upstream))
                    .with_state(UpstreamState {
                        requests: upstream_requests,
                        expected: upstream_expected,
                    }),
            )
            .await
            .unwrap();
        });

        let mut endpoint = CredentialBrokerEndpoint::new(format!(
            "http://{upstream_address}/provider/live?username=provider-user&password=provider-password"
        ));
        endpoint.headers_mut().insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer provider-secret"),
        );
        let arguments = Arc::new(Mutex::new(Vec::new()));
        let captured_arguments = Arc::clone(&arguments);
        let mut session = start_brokered(
            reqwest::Client::new(),
            endpoint,
            BrokeredInputFormat::DirectMpegTs,
            MpegTsRingConfig::new(8, 8).unwrap(),
            move |input, ring_config| {
                let input_argument = input.command_argument();
                captured_arguments
                    .lock()
                    .unwrap()
                    .push(input_argument.clone());
                spawn_command(
                    AuditedProcessCommand::test_only(
                        ProcessAdapterKind::Ffmpeg,
                        OsStr::new("curl"),
                        vec!["--fail".into(), "--silent".into(), input_argument],
                    ),
                    ring_config,
                )
            },
        )
        .await
        .unwrap();

        {
            let command_arguments = arguments.lock().unwrap();
            assert_eq!(command_arguments.len(), 1);
            let command_input = command_arguments[0].to_string_lossy();
            assert!(command_input.starts_with("http://127.0.0.1:"));
            for credential in ["provider-user", "provider-password", "provider-secret"] {
                assert!(!command_input.contains(credential));
            }
        }

        let mut cursor = session.subscribe();
        let actual = read_packets_to_end(&mut cursor).await;

        assert_eq!(actual, expected);
        assert_eq!(requests.load(Ordering::SeqCst), 1);
        assert_eq!(
            session.wait().await.unwrap(),
            ProcessExit::Completed {
                success: true,
                code: Some(0)
            }
        );
        let debug = format!("{session:?}");
        for credential in ["provider-user", "provider-password", "provider-secret"] {
            assert!(!debug.contains(credential));
        }
        upstream_task.abort();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn hls_brokered_process_receives_the_loopback_manifest_url() {
        let endpoint = CredentialBrokerEndpoint::new(
            "http://provider.invalid/live/master.m3u8?username=provider-user&password=provider-password",
        );
        let arguments = Arc::new(Mutex::new(Vec::new()));
        let captured_arguments = Arc::clone(&arguments);
        let session = start_brokered(
            reqwest::Client::new(),
            endpoint,
            BrokeredInputFormat::Hls(HlsBrokerConfig::default()),
            MpegTsRingConfig::new(8, 8).unwrap(),
            move |input, ring_config| {
                let input_argument = input.command_argument();
                captured_arguments
                    .lock()
                    .unwrap()
                    .push(input_argument.clone());
                spawn_command(
                    AuditedProcessCommand::test_only(
                        ProcessAdapterKind::Ffmpeg,
                        OsStr::new("/bin/sleep"),
                        vec!["30".into()],
                    ),
                    ring_config,
                )
            },
        )
        .await
        .unwrap();

        {
            let command_arguments = arguments.lock().unwrap();
            assert_eq!(command_arguments.len(), 1);
            let command_input = command_arguments[0].to_string_lossy();
            assert!(command_input.starts_with("http://127.0.0.1:"));
            assert!(command_input.contains("/hls/"));
            assert!(command_input.ends_with("/manifest.m3u8"));
            for credential in ["provider-user", "provider-password"] {
                assert!(!command_input.contains(credential));
            }
        }

        let debug = format!("{session:?}");
        for credential in ["provider-user", "provider-password"] {
            assert!(!debug.contains(credential));
        }
        assert!(matches!(
            timeout(Duration::from_secs(2), session.shutdown()).await,
            Ok(Ok(ProcessExit::Cancelled { .. }))
        ));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn leased_brokered_shutdown_retains_slot_until_child_and_broker_stop() {
        use std::convert::Infallible;

        #[derive(Clone)]
        struct UpstreamState {
            requests: Arc<AtomicUsize>,
        }

        async fn upstream(State(state): State<UpstreamState>) -> Body {
            state.requests.fetch_add(1, Ordering::SeqCst);
            let stream = async_stream::stream! {
                loop {
                    yield Ok::<_, Infallible>(Bytes::from_static(b"media"));
                    tokio::time::sleep(Duration::from_mins(1)).await;
                }
            };
            Body::from_stream(stream)
        }

        let requests = Arc::new(AtomicUsize::new(0));
        let upstream_listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let upstream_address = upstream_listener.local_addr().unwrap();
        let upstream_requests = Arc::clone(&requests);
        let upstream_task = tokio::spawn(async move {
            axum::serve(
                upstream_listener,
                Router::new()
                    .route("/provider/live", get(upstream))
                    .with_state(UpstreamState {
                        requests: upstream_requests,
                    }),
            )
            .await
            .unwrap();
        });

        let endpoint =
            CredentialBrokerEndpoint::new(format!("http://{upstream_address}/provider/live"));
        let session = start_brokered(
            reqwest::Client::new(),
            endpoint,
            BrokeredInputFormat::DirectMpegTs,
            MpegTsRingConfig::new(8, 8).unwrap(),
            |_input, ring_config| {
                spawn_command(
                    AuditedProcessCommand::test_only(
                        ProcessAdapterKind::Ffmpeg,
                        OsStr::new("/bin/sleep"),
                        vec!["30".into()],
                    ),
                    ring_config,
                )
            },
        )
        .await
        .unwrap();
        let broker_url = session
            .broker
            .as_ref()
            .unwrap()
            .input_source()
            .broker_url()
            .as_str()
            .to_owned();
        let response = reqwest::Client::new().get(broker_url).send().await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(requests.load(Ordering::SeqCst), 1);

        let slots = crate::ProviderSlotBroker::new("provider", 1);
        let lease = slots.try_acquire("channel").unwrap();
        let mut session = session.into_leased(lease);
        session.request_shutdown();

        assert!(
            timeout(Duration::from_millis(100), session.wait())
                .await
                .is_err(),
            "the active broker request must keep cleanup pending"
        );
        assert_eq!(slots.snapshot().active_sessions, 1);
        assert!(matches!(
            slots.try_acquire("replacement"),
            Err(crate::AcquireError::AtCapacity { .. })
        ));

        drop(response);
        let exit = timeout(Duration::from_secs(3), session.wait())
            .await
            .expect("cleanup must finish after the broker stops")
            .unwrap();
        assert!(matches!(exit, ProcessExit::Cancelled { .. }));
        assert_eq!(slots.snapshot().active_sessions, 0);
        assert!(slots.try_acquire("replacement").is_ok());
        upstream_task.abort();
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
