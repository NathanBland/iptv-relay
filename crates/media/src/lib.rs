//! Media-plane primitives for sharing IPTV upstreams safely.
//!
//! A shared upstream session can own a [`SlotLease`], publish packets through an
//! [`MpegTsRing`], and be deduplicated by a [`SharedSessionRegistry`]. Native
//! HTTP and fixed `FFmpeg`/VLC adapters provide the corresponding input edges.

pub mod broker;
pub mod process;
mod psi;
pub mod recovery;
pub mod registry;
pub mod ring;
pub mod session;

pub use broker::{AcquireError, PoolSnapshot, ProviderSlotBroker, SlotLease};
pub use process::{
    AuditedProcessCommand, FfmpegInputAdapter, InputSource, InputSourceError, ProcessAdapterError,
    ProcessAdapterKind, ProcessExit, ProcessFailureStage, ProcessInputSession, VlcInputAdapter,
};
pub use recovery::{
    HttpTsSessionSnapshot, MAX_RECOVERY_WINDOW, RecoveryConfigError, RecoveryPolicy,
    SessionFailureKind, SessionState,
};
pub use registry::SharedSessionRegistry;
pub use ring::{
    DEFAULT_PRE_ROLL, MPEG_TS_PACKET_SIZE, MpegTsRing, MpegTsRingConfig, RingCloseReason,
    RingConfigError, RingCursor, RingRead, RingReadError, RingSnapshot, RingWriteError,
    WriteOutcome,
};
pub use session::{
    HttpTsEndpoint, HttpTsSessionKey, HttpTsSessionManager, HttpTsSourceSpec, MpegTsPacketizer,
    PacketizerError, ProviderSpec, SessionStartError, ViewerByteStream, ViewerHandle,
    ViewerStreamError,
};
