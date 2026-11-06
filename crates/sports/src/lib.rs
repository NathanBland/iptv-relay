//! ESPN sports metadata: league table, cached client, and pure event matcher.
//!
//! This crate has no dependency on persistence or the API. The matcher in
//! [`matcher`] is pure. The client in [`client`] performs public `GET` requests.

pub mod art;
pub mod client;
pub mod league;
pub mod matcher;
pub mod model;

pub use client::{EspnClient, EspnClientConfig, EspnError};
pub use league::League;
pub use matcher::{EventMatch, MatchQuery, MatchedTeam, Separator, find_team, match_event};
pub use model::{EspnEvent, EspnTeam};
