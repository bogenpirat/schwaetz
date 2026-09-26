//! Sans-IO IRC session for schwätz.
//!
//! [`Session`] consumes parsed [`schwaetz_proto::Message`]s and produces outgoing messages and
//! semantic [`SessionEvent`]s. It never touches sockets or clocks, which keeps the whole IRC
//! state machine deterministic and unit-testable.

pub mod event;
pub mod sasl;
pub mod session;
pub mod state;

pub use event::{Chat, ChatKind, Event, SessionEvent, StandardReplyKind, Target, TwitchEvent};
pub use sasl::SaslConfig;
pub use session::{Phase, Session, SessionConfig};
pub use state::{Channel, Member, ModeChange, ModeListEntry, User, WhoisInfo};
