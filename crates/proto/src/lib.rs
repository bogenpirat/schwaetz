//! IRC wire format for schwätz.
//!
//! Covers RFC 1459/2812, the Modern IRC documents and IRCv3 message tags. Everything here is
//! pure and allocation-conscious so it can run on the network thread at high message rates.

pub mod casemap;
pub mod ctcp;
pub mod decode;
pub mod format;
pub mod isupport;
pub mod mask;
pub mod message;
pub mod numeric;
pub mod split;
pub mod tags;

pub use casemap::CaseMapping;
pub use isupport::ISupport;
pub use message::{Message, MessageRef, ParseError, Source};
pub use tags::Tags;

/// Maximum length of an IRC line excluding tags, including the trailing CRLF.
pub const MAX_LINE_LEN: usize = 512;
/// Maximum length of the client-sent tag section, including the leading `@` and trailing space.
pub const MAX_CLIENT_TAGS_LEN: usize = 4096;
