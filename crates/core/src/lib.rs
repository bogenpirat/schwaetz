//! Application model for schwätz: networks, buffers, commands and configuration.
//!
//! [`App`] is UI-agnostic. The UI feeds it network events, user input and clock ticks, then reads
//! buffers and drains [`Effect`]s (notifications, persistence) and network commands.

pub mod app;
pub mod badges;
pub mod buffer;
pub mod commands;
pub mod completion;
pub mod config;
pub mod emote_providers;
pub mod emotes;
pub mod filter;
pub mod helix;
pub mod imgur;
pub mod paths;
pub mod secrets;
pub mod services;
pub mod time;
pub mod twitch;
pub mod twitch_auth;
pub mod znc;

pub use app::{App, ConnState, Dirty, Effect, Network, TopicParts};
pub use buffer::{Activity, Buffer, BufferId, BufferKind, Line, LineExtra, LineFlags, LineKind, NotifyLevel};
pub use config::{Config, NetworkConfig, NetworkKind};
pub use paths::Paths;
