//! Message history for schwätz.
//!
//! Lines are written by a background thread in batched transactions to a SQLite database (WAL
//! mode, FTS5 index over message text) and, optionally, appended to plain-text log files
//! (`logs/<network>/<buffer>/<date>.log`). Reads (scroll-back, search) use a separate connection
//! on the calling thread, which WAL allows to run concurrently with writes.

use rusqlite::{Connection, OptionalExtension, params};
use schwaetz_core::buffer::{Line, LineExtra, LineFlags, LineKind};
use schwaetz_core::services::HistoryStore;
use schwaetz_core::time;
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

const SCHEMA: &str = r#"
PRAGMA journal_mode = WAL;
PRAGMA synchronous = NORMAL;
CREATE TABLE IF NOT EXISTS buffers (
    id INTEGER PRIMARY KEY,
    network TEXT NOT NULL,
    name TEXT NOT NULL COLLATE NOCASE,
    UNIQUE (network, name)
);
CREATE TABLE IF NOT EXISTS messages (
    id INTEGER PRIMARY KEY,
    buffer INTEGER NOT NULL REFERENCES buffers(id) ON DELETE CASCADE,
    time INTEGER NOT NULL,
    kind INTEGER NOT NULL,
    flags INTEGER NOT NULL,
    nick TEXT NOT NULL,
    prefix TEXT,
    text TEXT NOT NULL,
    msgid TEXT,
    display TEXT,
    color INTEGER
);
CREATE INDEX IF NOT EXISTS messages_buffer_time ON messages (buffer, time);
CREATE INDEX IF NOT EXISTS messages_msgid ON messages (msgid) WHERE msgid IS NOT NULL;
CREATE VIRTUAL TABLE IF NOT EXISTS messages_fts USING fts5 (
    text, content = 'messages', content_rowid = 'id', tokenize = 'unicode61 remove_diacritics 2'
);
CREATE TRIGGER IF NOT EXISTS messages_ai AFTER INSERT ON messages WHEN new.kind <= 2 BEGIN
    INSERT INTO messages_fts (rowid, text) VALUES (new.id, new.text);
END;
CREATE TRIGGER IF NOT EXISTS messages_ad AFTER DELETE ON messages WHEN old.kind <= 2 BEGIN
    INSERT INTO messages_fts (messages_fts, rowid, text) VALUES ('delete', old.id, old.text);
END;
PRAGMA user_version = 1;
"#;

fn kind_code(k: LineKind) -> i64 {
    match k {
        LineKind::Message => 0,
        LineKind::Action => 1,
        LineKind::Notice => 2,
        LineKind::Join => 3,
        LineKind::Part => 4,
        LineKind::Quit => 5,
        LineKind::Kick => 6,
        LineKind::Nick => 7,
        LineKind::Mode => 8,
        LineKind::Topic => 9,
        LineKind::Invite => 10,
        LineKind::Status => 11,
        LineKind::Error => 12,
        LineKind::Server => 13,
        LineKind::Motd => 14,
        LineKind::Ctcp => 15,
        LineKind::System => 16,
        LineKind::Netsplit => 17,
    }
}

fn kind_from(c: i64) -> LineKind {
    match c {
        0 => LineKind::Message,
        1 => LineKind::Action,
        2 => LineKind::Notice,
        3 => LineKind::Join,
        4 => LineKind::Part,
        5 => LineKind::Quit,
        6 => LineKind::Kick,
        7 => LineKind::Nick,
        8 => LineKind::Mode,
        9 => LineKind::Topic,
        10 => LineKind::Invite,
        11 => LineKind::Status,
        12 => LineKind::Error,
        13 => LineKind::Server,
        14 => LineKind::Motd,
        15 => LineKind::Ctcp,
        16 => LineKind::System,
        _ => LineKind::Netsplit,
    }
}

enum Op {
    Log { network: String, buffer: String, line: Line },
    Flush(mpsc::Sender<()>),
    Prune { older_than: i64 },
}

/// Handle used by the UI thread. Implements [`HistoryStore`].
pub struct Store {
    tx: mpsc::Sender<Op>,
    reader: Connection,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Store {
    /// Opens (creating if needed) the database. `logs_dir` enables plain-text logs.
    pub fn open(db: &Path, logs_dir: Option<PathBuf>) -> Result<Store, String> {
        if let Some(dir) = db.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let writer = Connection::open(db).map_err(|e| format!("{}: {e}", db.display()))?;
        writer.execute_batch(SCHEMA).map_err(|e| format!("history schema: {e}"))?;
        let reader = Connection::open(db).map_err(|e| e.to_string())?;
        reader.busy_timeout(Duration::from_millis(500)).map_err(|e| e.to_string())?;
        let (tx, rx) = mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("schwaetz-store".into())
            .stack_size(256 * 1024)
            .spawn(move || writer_loop(writer, rx, logs_dir))
            .map_err(|e| e.to_string())?;
        Ok(Store { tx, reader, thread: Some(thread) })
    }

    /// Deletes messages older than `days` (0 = keep forever).
    pub fn prune_days(&self, days: u32) {
        if days > 0 {
            let cutoff = time::now_ms() - days as i64 * 86_400_000;
            let _ = self.tx.send(Op::Prune { older_than: cutoff });
        }
    }

    fn row_to_line(r: &rusqlite::Row) -> rusqlite::Result<Line> {
        let prefix: Option<String> = r.get(5)?;
        let msgid: Option<String> = r.get(7)?;
        let display: Option<String> = r.get(8)?;
        let color: Option<i64> = r.get(9)?;
        let extra = if msgid.is_some() || display.is_some() || color.is_some() {
            Some(Box::new(LineExtra {
                msgid,
                display_name: display,
                color: color.map(|c| c as u32),
                ..Default::default()
            }))
        } else {
            None
        };
        Ok(Line {
            id: r.get::<_, i64>(0)? as u64,
            time: r.get(1)?,
            kind: kind_from(r.get(2)?),
            flags: LineFlags(r.get::<_, i64>(3)? as u16 & !(LineFlags::FILTERED)),
            nick: r.get::<_, String>(4)?.into(),
            prefix: prefix.and_then(|p| p.chars().next()),
            text: r.get::<_, String>(6)?.into(),
            extra,
        })
    }
}

impl Drop for Store {
    fn drop(&mut self) {
        let (tx, rx) = mpsc::channel();
        if self.tx.send(Op::Flush(tx)).is_ok() {
            let _ = rx.recv_timeout(Duration::from_secs(3));
        }
        // Closing the channel ends the writer thread.
        let (dummy, _) = mpsc::channel();
        self.tx = dummy;
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

const SELECT_COLS: &str = "m.id, m.time, m.kind, m.flags, m.nick, m.prefix, m.text, m.msgid, m.display, m.color";

impl HistoryStore for Store {
    fn log(&mut self, network: &str, buffer: &str, line: &Line) {
        let _ = self.tx.send(Op::Log { network: network.to_owned(), buffer: buffer.to_owned(), line: line.clone() });
    }

    fn load_before(&mut self, network: &str, buffer: &str, before: i64, limit: usize) -> Vec<Line> {
        let sql = format!(
            "SELECT {SELECT_COLS} FROM messages m JOIN buffers b ON b.id = m.buffer
             WHERE b.network = ?1 AND b.name = ?2 AND m.time < ?3 ORDER BY m.time DESC, m.id DESC LIMIT ?4"
        );
        let r = self.reader.prepare_cached(&sql).and_then(|mut st| {
            st.query_map(params![network, buffer, before, limit as i64], Store::row_to_line)?
                .collect::<Result<Vec<_>, _>>()
        });
        match r {
            Ok(mut v) => {
                v.reverse();
                v
            }
            Err(e) => {
                tracing::warn!("history load failed: {e}");
                Vec::new()
            }
        }
    }

    fn search(
        &mut self,
        query: &str,
        network: Option<&str>,
        buffer: Option<&str>,
        limit: usize,
    ) -> Vec<(String, String, Line)> {
        // Quote each term so user input can't produce FTS syntax errors; prefix-match the last.
        let terms: Vec<String> = query.split_whitespace().map(|t| format!("\"{}\"", t.replace('"', "\"\""))).collect();
        if terms.is_empty() {
            return Vec::new();
        }
        let mut fts = terms.join(" ");
        fts.push('*');
        let sql = format!(
            "SELECT {SELECT_COLS}, b.network, b.name FROM messages_fts f
             JOIN messages m ON m.id = f.rowid JOIN buffers b ON b.id = m.buffer
             WHERE messages_fts MATCH ?1 AND (?2 IS NULL OR b.network = ?2) AND (?3 IS NULL OR b.name = ?3)
             ORDER BY m.time DESC LIMIT ?4"
        );
        let r = self.reader.prepare_cached(&sql).and_then(|mut st| {
            st.query_map(params![fts, network, buffer, limit as i64], |r| {
                Ok((r.get::<_, String>(10)?, r.get::<_, String>(11)?, Store::row_to_line(r)?))
            })?
            .collect::<Result<Vec<_>, _>>()
        });
        r.unwrap_or_else(|e| {
            tracing::warn!("history search failed: {e}");
            Vec::new()
        })
    }

    fn last_seen(&mut self, network: &str) -> Option<i64> {
        self.reader
            .query_row(
                "SELECT max(m.time) FROM messages m JOIN buffers b ON b.id = m.buffer WHERE b.network = ?1 AND m.kind <= 2",
                params![network],
                |r| r.get::<_, Option<i64>>(0),
            )
            .optional()
            .ok()
            .flatten()
            .flatten()
    }

    fn flush(&mut self) {
        let (tx, rx) = mpsc::channel();
        if self.tx.send(Op::Flush(tx)).is_ok() {
            let _ = rx.recv_timeout(Duration::from_secs(3));
        }
    }
}

struct TextLogs {
    dir: PathBuf,
    files: HashMap<(String, String, i64), BufWriter<File>>,
}

fn sanitize(name: &str) -> String {
    let s: String = name
        .chars()
        .map(|c| if c.is_control() || "<>:\"/\\|?*".contains(c) { '_' } else { c })
        .collect::<String>()
        .trim_end_matches(['.', ' '])
        .to_owned();
    // Windows reserved device names.
    let upper = s.to_ascii_uppercase();
    let base = upper.split('.').next().unwrap_or("");
    if matches!(base, "CON" | "PRN" | "AUX" | "NUL")
        || (base.len() == 4 && (base.starts_with("COM") || base.starts_with("LPT")))
    {
        format!("_{s}")
    } else if s.is_empty() {
        "_".into()
    } else {
        s
    }
}

impl TextLogs {
    fn write(&mut self, network: &str, buffer: &str, line: &Line) {
        let t = time::local(line.time);
        let day = time::local_day(line.time);
        let key = (network.to_owned(), buffer.to_lowercase(), day);
        if !self.files.contains_key(&key) {
            if self.files.len() > 64 {
                self.files.clear();
            }
            let dir = self.dir.join(sanitize(network)).join(sanitize(buffer));
            if std::fs::create_dir_all(&dir).is_err() {
                return;
            }
            let path = dir.join(format!("{:04}-{:02}-{:02}.log", t.year, t.month, t.day));
            let Ok(f) = OpenOptions::new().create(true).append(true).open(path) else { return };
            self.files.insert(key.clone(), BufWriter::new(f));
        }
        let text = schwaetz_proto::format::strip(&line.text);
        let ts = time::format("%H:%M:%S", t);
        let nick = line.display_nick();
        let out = match line.kind {
            LineKind::Message => format!("[{ts}] <{}{nick}> {text}", line.prefix.map(String::from).unwrap_or_default()),
            LineKind::Action => format!("[{ts}] * {nick} {text}"),
            LineKind::Notice => format!("[{ts}] -{nick}- {text}"),
            _ => format!("[{ts}] -!- {text}"),
        };
        if let Some(f) = self.files.get_mut(&key) {
            let _ = writeln!(f, "{}", out.replace(['\r', '\n'], " "));
        }
    }

    fn flush(&mut self) {
        for f in self.files.values_mut() {
            let _ = f.flush();
        }
    }
}

fn writer_loop(mut db: Connection, rx: mpsc::Receiver<Op>, logs_dir: Option<PathBuf>) {
    let mut logs = logs_dir.map(|dir| TextLogs { dir, files: HashMap::new() });
    let mut buffer_ids: HashMap<(String, String), i64> = HashMap::new();
    let mut pending: Vec<(String, String, Line)> = Vec::new();
    loop {
        // Block for the first op, then gather whatever arrives within the batch window.
        let first = match rx.recv() {
            Ok(op) => op,
            Err(_) => break,
        };
        let mut ops = vec![first];
        let deadline = std::time::Instant::now() + Duration::from_millis(250);
        while let Some(left) = deadline.checked_duration_since(std::time::Instant::now()) {
            match rx.recv_timeout(left) {
                Ok(op) => {
                    let flush = matches!(op, Op::Flush(_));
                    ops.push(op);
                    if flush {
                        break;
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => break,
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        let mut acks = Vec::new();
        for op in ops {
            match op {
                Op::Log { network, buffer, line } => pending.push((network, buffer, line)),
                Op::Flush(ack) => acks.push(ack),
                Op::Prune { older_than } => {
                    let _ = db.execute("DELETE FROM messages WHERE time < ?1", params![older_than]);
                }
            }
        }
        if !pending.is_empty() {
            if let Err(e) = write_batch(&mut db, &mut buffer_ids, &pending) {
                tracing::warn!("history write failed: {e}");
            }
            if let Some(l) = logs.as_mut() {
                for (n, b, line) in &pending {
                    l.write(n, b, line);
                }
                l.flush();
            }
            pending.clear();
        }
        for a in acks {
            let _ = a.send(());
        }
    }
    if let Some(l) = logs.as_mut() {
        l.flush();
    }
}

fn write_batch(
    db: &mut Connection,
    ids: &mut HashMap<(String, String), i64>,
    batch: &[(String, String, Line)],
) -> rusqlite::Result<()> {
    let tx = db.transaction()?;
    {
        let mut ins_buf = tx.prepare_cached("INSERT OR IGNORE INTO buffers (network, name) VALUES (?1, ?2)")?;
        let mut get_buf = tx.prepare_cached("SELECT id FROM buffers WHERE network = ?1 AND name = ?2")?;
        let mut exists = tx.prepare_cached("SELECT 1 FROM messages WHERE msgid = ?1 AND buffer = ?2 LIMIT 1")?;
        let mut ins = tx.prepare_cached(
            "INSERT INTO messages (buffer, time, kind, flags, nick, prefix, text, msgid, display, color)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        )?;
        for (network, buffer, line) in batch {
            let key = (network.clone(), buffer.to_lowercase());
            let bid = match ids.get(&key) {
                Some(id) => *id,
                None => {
                    ins_buf.execute(params![network, buffer])?;
                    let id: i64 = get_buf.query_row(params![network, buffer], |r| r.get(0))?;
                    ids.insert(key, id);
                    id
                }
            };
            let msgid = line.msgid();
            if let Some(m) = msgid
                && exists.exists(params![m, bid])?
            {
                continue;
            }
            let extra = line.extra.as_deref();
            ins.execute(params![
                bid,
                line.time,
                kind_code(line.kind),
                line.flags.0 as i64,
                &*line.nick,
                line.prefix.map(String::from),
                &*line.text,
                msgid,
                extra.and_then(|e| e.display_name.as_deref()),
                extra.and_then(|e| e.color).map(|c| c as i64),
            ])?;
        }
    }
    tx.commit()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_filesystem_safe() {
        assert_eq!(sanitize("#chan"), "#chan");
        assert_eq!(sanitize("a/b:c"), "a_b_c");
        assert_eq!(sanitize("CON"), "_CON");
        assert_eq!(sanitize("com1.txt"), "_com1.txt");
        assert_eq!(sanitize("trailing. "), "trailing");
    }
}
