//! Interfaces for optional services the shell wires in (history storage, scripting).

use crate::app::App;
use crate::buffer::{BufferId, Line};

/// Persistent message history.
pub trait HistoryStore {
    fn log(&mut self, network: &str, buffer: &str, line: &Line);
    /// Lines older than `before` (Unix ms), oldest first.
    fn load_before(&mut self, network: &str, buffer: &str, before: i64, limit: usize) -> Vec<Line>;
    /// Full-text search; returns (network, buffer, line), newest first.
    fn search(
        &mut self,
        query: &str,
        network: Option<&str>,
        buffer: Option<&str>,
        limit: usize,
    ) -> Vec<(String, String, Line)>;
    /// Newest logged message time for a network (ZNC playback / chathistory gap-fill).
    fn last_seen(&mut self, network: &str) -> Option<i64>;
    /// Flushes pending writes (called on shutdown).
    fn flush(&mut self) {}
}

/// One script file as the settings page shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScriptInfo {
    /// File name without extension; also the name used in `[scripts] disabled`.
    pub name: String,
    pub file: String,
    pub enabled: bool,
    pub running: bool,
    /// Why it is not running (load or compile error), or the latest runtime error.
    pub error: Option<String>,
    /// Has `// @grant http`.
    pub network: bool,
    pub commands: Vec<String>,
}

/// A scripting host. All calls happen on the UI thread, after the model has been updated, so
/// scripts can freely inspect and modify it.
pub trait ScriptHost {
    /// Lines added since the last call, as (buffer, line id).
    fn on_lines(&mut self, app: &mut App, lines: &[(BufferId, u64)]);
    /// Called for input before command processing. Returning `true` consumes it.
    fn on_input(&mut self, app: &mut App, buffer: BufferId, text: &str) -> bool;
    /// Raw IRC line received from a network (before the model processes it).
    fn on_raw(&mut self, app: &mut App, network: &str, line: &schwaetz_proto::Message);
    /// Periodic tick (≈1s) for timers.
    fn tick(&mut self, app: &mut App, now: i64);
    fn reload(&mut self, app: &mut App);
    /// Script-registered command names (for completion and /help).
    fn commands(&self) -> Vec<String>;
    /// Every script file in the scripts folder with its state (for the settings page).
    fn list(&self, app: &App) -> Vec<ScriptInfo>;
    /// Applies file and on/off changes now instead of at the next periodic scan.
    fn refresh(&mut self, app: &mut App);
    /// The scripts folder.
    fn dir(&self) -> std::path::PathBuf;
    /// Copies the bundled example scripts that are not there yet into the scripts folder;
    /// returns their names.
    fn install_examples(&mut self) -> Result<Vec<String>, String>;
}
