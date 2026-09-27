//! Native Win32 + Direct2D user interface for schwätz.

pub mod anim;
pub mod chat;
pub mod editor;
pub mod gfx;
pub mod lists;
pub mod overlay;
pub mod shell;
pub mod text;
pub mod theme;
pub mod win;

pub use shell::{COPYDATA_MAGIC, Services, run};
pub mod a11y;
pub mod form;
pub mod images;
pub mod session;
