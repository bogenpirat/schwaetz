//! Media for schwätz: fetches images and web pages on worker threads, decodes images with WIC
//! into premultiplied BGRA (scaled down to a maximum size) and extracts OpenGraph metadata for
//! link previews. Downloads are cached on disk.

mod og;
mod wic;

pub use og::PageMeta;
pub use wic::Frame;

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// An image URL (emotes, badges, direct image links).
    Image { max_dim: u32 },
    /// Any URL: images are decoded directly, HTML pages yield OpenGraph metadata.
    Preview { max_dim: u32 },
}

#[derive(Debug)]
pub enum MediaResult {
    /// Decoded image; animated images have several frames.
    Image {
        url: String,
        width: u32,
        height: u32,
        frames: Vec<Frame>,
    },
    Page {
        url: String,
        meta: PageMeta,
    },
    Failed {
        url: String,
        error: String,
    },
}

pub struct Limits {
    pub max_image_bytes: u64,
    pub max_page_bytes: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Limits { max_image_bytes: 10 << 20, max_page_bytes: 512 << 10 }
    }
}

struct Job {
    url: String,
    kind: Kind,
}

/// Handle owned by the UI thread.
pub struct Media {
    tx: Option<mpsc::Sender<Job>>,
    rx: mpsc::Receiver<MediaResult>,
    pending: Arc<AtomicBool>,
    in_flight: HashSet<String>,
    workers: Vec<std::thread::JoinHandle<()>>,
}

impl Media {
    /// Starts `threads` workers. `wake` is called (coalesced) when results are ready.
    pub fn start(cache_dir: Option<PathBuf>, threads: usize, wake: impl Fn() + Send + Sync + 'static) -> Media {
        let (tx, jobs) = mpsc::channel::<Job>();
        let jobs = Arc::new(Mutex::new(jobs));
        let (res_tx, rx) = mpsc::channel();
        let pending = Arc::new(AtomicBool::new(false));
        let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(wake);
        let cache = cache_dir.map(|d| d.join("media"));
        if let Some(c) = &cache {
            let _ = std::fs::create_dir_all(c);
            prune_cache(c, 200 << 20);
        }
        let workers = (0..threads.max(1))
            .map(|i| {
                let jobs = jobs.clone();
                let res_tx = res_tx.clone();
                let pending = pending.clone();
                let wake = wake.clone();
                let cache = cache.clone();
                std::thread::Builder::new()
                    .name(format!("schwaetz-media-{i}"))
                    .stack_size(512 * 1024)
                    .spawn(move || {
                        wic::init_thread();
                        loop {
                            let job = match jobs.lock().map(|j| j.recv()) {
                                Ok(Ok(job)) => job,
                                _ => break,
                            };
                            let r = process(&job, cache.as_ref(), &Limits::default());
                            if res_tx.send(r).is_ok() && !pending.swap(true, Ordering::AcqRel) {
                                wake();
                            }
                        }
                    })
                    .expect("media worker")
            })
            .collect();
        Media { tx: Some(tx), rx, pending, in_flight: HashSet::new(), workers }
    }

    /// Queues a request unless the same URL is already being fetched.
    pub fn request(&mut self, url: &str, kind: Kind) {
        if self.in_flight.insert(url.to_owned())
            && let Some(tx) = &self.tx
        {
            let _ = tx.send(Job { url: url.to_owned(), kind });
        }
    }

    pub fn is_pending(&self, url: &str) -> bool {
        self.in_flight.contains(url)
    }

    pub fn drain(&mut self) -> Vec<MediaResult> {
        self.pending.store(false, Ordering::Release);
        let out: Vec<MediaResult> = self.rx.try_iter().collect();
        for r in &out {
            let url = match r {
                MediaResult::Image { url, .. } | MediaResult::Page { url, .. } | MediaResult::Failed { url, .. } => url,
            };
            self.in_flight.remove(url);
        }
        out
    }
}

impl Drop for Media {
    fn drop(&mut self) {
        self.tx = None;
        // Workers may be blocked in a download; don't wait for them.
        self.workers.clear();
    }
}

fn cache_key(url: &str) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in url.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{h:016x}")
}

/// Removes the oldest cached files until the cache is below `max` bytes.
fn prune_cache(dir: &std::path::Path, max: u64) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut files: Vec<(std::time::SystemTime, u64, PathBuf)> = rd
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let m = e.metadata().ok()?;
            Some((m.modified().ok()?, m.len(), e.path()))
        })
        .collect();
    let mut total: u64 = files.iter().map(|f| f.1).sum();
    files.sort();
    for (_, len, path) in files {
        if total <= max {
            break;
        }
        if std::fs::remove_file(&path).is_ok() {
            total -= len;
        }
    }
}

fn fetch(url: &str, max: u64, cache: Option<&PathBuf>) -> Result<(String, Vec<u8>), String> {
    let key = cache.map(|c| c.join(cache_key(url)));
    if let Some(k) = &key
        && let Ok(bytes) = std::fs::read(k)
    {
        // Cached entries store "content-type\n" + body.
        if let Some(pos) = bytes.iter().position(|&b| b == b'\n') {
            let ct = String::from_utf8_lossy(&bytes[..pos]).into_owned();
            return Ok((ct, bytes[pos + 1..].to_vec()));
        }
    }
    let r = schwaetz_net::http::get(url, max, false)?;
    if !(200..300).contains(&r.status) {
        return Err(format!("HTTP {}", r.status));
    }
    if let Some(k) = &key {
        let mut data = r.content_type.clone().into_bytes();
        data.push(b'\n');
        data.extend_from_slice(&r.body);
        let _ = std::fs::write(k, data);
    }
    Ok((r.content_type, r.body))
}

fn process(job: &Job, cache: Option<&PathBuf>, limits: &Limits) -> MediaResult {
    let url = job.url.clone();
    let fail = |e: String| MediaResult::Failed { url: url.clone(), error: e };
    let (max_dim, preview) = match job.kind {
        Kind::Image { max_dim } => (max_dim, false),
        Kind::Preview { max_dim } => (max_dim, true),
    };
    let limit = if preview { limits.max_image_bytes.max(limits.max_page_bytes) } else { limits.max_image_bytes };
    let (ct, body) = match fetch(&url, limit, cache) {
        Ok(v) => v,
        Err(e) => return fail(e),
    };
    let is_html = ct.starts_with("text/html") || ct.starts_with("application/xhtml");
    if preview && is_html {
        let text = String::from_utf8_lossy(&body[..body.len().min(limits.max_page_bytes as usize)]);
        return match og::parse(&text, &url) {
            Some(meta) => MediaResult::Page { url, meta },
            None => fail("no preview information".into()),
        };
    }
    if !ct.is_empty() && !ct.starts_with("image/") && !ct.starts_with("application/octet-stream") {
        return fail(format!("not an image ({ct})"));
    }
    match wic::decode_frames(&body, max_dim) {
        Ok((width, height, frames)) => MediaResult::Image { url, width, height, frames },
        Err(e) => fail(e),
    }
}

/// Decodes image bytes directly (used by tests and for data already in memory).
pub fn decode(bytes: &[u8], max_dim: u32) -> Result<(u32, u32, Vec<u8>), String> {
    wic::init_thread();
    wic::decode(bytes, max_dim)
}

/// Decodes all frames of image bytes (used by tests).
pub fn decode_frames(bytes: &[u8], max_dim: u32) -> Result<(u32, u32, Vec<Frame>), String> {
    wic::init_thread();
    wic::decode_frames(bytes, max_dim)
}

/// Re-encodes an image file as PNG (clipboard bitmaps before they are uploaded).
pub fn to_png(bytes: &[u8]) -> Result<Vec<u8>, String> {
    wic::init_thread();
    wic::to_png(bytes)
}
