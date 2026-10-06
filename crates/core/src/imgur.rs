//! Anonymous image uploads to Imgur (blocking HTTPS; call from worker threads only).
//!
//! Imgur's API wants a Client-ID. It comes from `[uploads] imgur_client_id`, else from the build
//! (`SCHWAETZ_IMGUR_CLIENT_ID`), else it is the one Imgur's own website uploads with, read from
//! the site's script at the first upload. That last one is not an official guest ID: it can
//! change or stop working at any time, which is why the other two take precedence.

use std::io::Read;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

/// Imgur rejects larger images.
pub const MAX_IMAGE_BYTES: u64 = 20 << 20;
const API: &str = "https://api.imgur.com/3";
const MAX_BODY: u64 = 64 << 10;
const BOUNDARY: &str = "----schwaetz-7f3a9c1e5b2d4086";

/// The Client-ID read from imgur.com, kept for the rest of the session.
static SCRAPED: Mutex<Option<String>> = Mutex::new(None);

/// The Imgur application compiled into this build (`SCHWAETZ_IMGUR_CLIENT_ID`), if any.
pub fn built_in_client_id() -> Option<&'static str> {
    option_env!("SCHWAETZ_IMGUR_CLIENT_ID").map(str::trim).filter(|s| !s.is_empty())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Uploaded {
    /// Direct link to the image file (`https://i.imgur.com/<id>.<ext>`).
    pub link: String,
    /// Deletes the image again (see [`delete`]).
    pub deletehash: String,
    /// The Client-ID it was uploaded with.
    pub client_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UploadError {
    /// The cancel flag was set before the upload went through.
    Cancelled,
    Failed(String),
}

/// File extensions (lowercase) of the images Imgur takes, with their media types.
const TYPES: &[(&str, &str)] = &[
    ("png", "image/png"),
    ("jpg", "image/jpeg"),
    ("jpeg", "image/jpeg"),
    ("gif", "image/gif"),
    ("webp", "image/webp"),
    ("bmp", "image/bmp"),
];

fn media_type(file_name: &str) -> Option<&'static str> {
    let ext = file_name.rsplit_once('.')?.1.to_ascii_lowercase();
    TYPES.iter().find(|(e, _)| *e == ext).map(|(_, t)| *t)
}

/// Whether a file name looks like an image that can be uploaded.
pub fn is_image_name(file_name: &str) -> bool {
    media_type(file_name).is_some()
}

/// The URL of the website's main script, from the HTML of imgur.com.
fn bundle_url(html: &str) -> Option<String> {
    let at = html.find("desktop-assets/js/main.")?;
    // The attribute may be unquoted (`src=https://…/main.<hash>.js>`).
    let start = html[..at].rfind(['"', '\'', '=', ' '])? + 1;
    let end = at + html[at..].find(['"', '\'', '>', ' '])?;
    let url = &html[start..end];
    let url = if url.starts_with("//") { format!("https:{url}") } else { url.to_owned() };
    (url.starts_with("https://") && url.ends_with(".js")).then_some(url)
}

/// The Client-ID the website's script calls the API with.
fn bundle_client_id(js: &str) -> Option<String> {
    const KEY: &str = "apiClientId:\"";
    let rest = &js[js.find(KEY)? + KEY.len()..];
    let id = &rest[..rest.find('"')?];
    ((8..=32).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_hexdigit())).then(|| id.to_owned())
}

fn scrape_client_id() -> Result<String, String> {
    let fail = |what: &str| format!("could not find Imgur's client ID ({what}); set [uploads] imgur_client_id");
    let page = schwaetz_net::http::get("https://imgur.com/", 4 << 20, false).map_err(|e| fail(&e))?;
    let url = bundle_url(&String::from_utf8_lossy(&page.body)).ok_or_else(|| fail("no script"))?;
    let js = schwaetz_net::http::get(&url, 16 << 20, false).map_err(|e| fail(&e))?;
    bundle_client_id(&String::from_utf8_lossy(&js.body)).ok_or_else(|| fail("not in the script"))
}

/// The Client-ID to use and whether it is the website's (which may have gone stale).
pub fn client_id(configured: &str) -> Result<(String, bool), String> {
    let configured = configured.trim();
    if !configured.is_empty() {
        return Ok((configured.to_owned(), false));
    }
    if let Some(id) = built_in_client_id() {
        return Ok((id.to_owned(), false));
    }
    let mut cached = SCRAPED.lock().unwrap_or_else(|e| e.into_inner());
    if cached.is_none() {
        *cached = Some(scrape_client_id()?);
    }
    Ok((cached.clone().unwrap_or_default(), true))
}

fn multipart(image: &[u8], file_name: &str) -> Vec<u8> {
    let name: String = file_name.chars().filter(|c| !c.is_control() && *c != '"' && *c != '\\').collect();
    let kind = media_type(file_name).unwrap_or("application/octet-stream");
    let mut body = Vec::with_capacity(image.len() + 256);
    body.extend_from_slice(
        format!(
            "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"image\"; filename=\"{name}\"\r\n\
             Content-Type: {kind}\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(image);
    body.extend_from_slice(format!("\r\n--{BOUNDARY}--\r\n").as_bytes());
    body
}

/// Hands out the request body in small pieces, counting them and failing once cancelled.
struct Body<'a> {
    data: &'a [u8],
    pos: usize,
    cancel: &'a AtomicBool,
    sent: &'a AtomicU64,
}

impl Read for Body<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.cancel.load(Ordering::Relaxed) {
            return Err(std::io::Error::other("cancelled"));
        }
        let n = buf.len().min(16 << 10).min(self.data.len() - self.pos);
        buf[..n].copy_from_slice(&self.data[self.pos..self.pos + n]);
        self.pos += n;
        self.sent.store(self.pos as u64, Ordering::Relaxed);
        Ok(n)
    }
}

/// Imgur's answer to an upload: the link and delete hash, or its error message.
fn parse_upload(status: u16, body: &[u8]) -> Result<(String, String), String> {
    let v: serde_json::Value = serde_json::from_slice(body).map_err(|_| format!("Imgur answered HTTP {status}"))?;
    let data = &v["data"];
    if let (true, Some(link)) = (v["success"].as_bool().unwrap_or(false), data["link"].as_str()) {
        return Ok((link.to_owned(), data["deletehash"].as_str().unwrap_or("").to_owned()));
    }
    if status == 429 {
        // Also its answer to a Client-ID it does not know.
        return Err("Imgur: too many uploads, or the client ID is not accepted (HTTP 429)".into());
    }
    let error = &data["error"];
    let message = (error.as_str().or_else(|| error["message"].as_str()))
        .or_else(|| v["errors"][0]["detail"].as_str())
        .unwrap_or("upload rejected");
    Err(format!("Imgur: {message} (HTTP {status})"))
}

fn upload_with(
    client_id: &str,
    body: &[u8],
    cancel: &AtomicBool,
    sent: &AtomicU64,
) -> Result<(String, String), (u16, String)> {
    sent.store(0, Ordering::Relaxed);
    let mut reader = Body { data: body, pos: 0, cancel, sent };
    let r = schwaetz_net::http::post_body(
        &format!("{API}/image"),
        &[("Authorization", &format!("Client-ID {client_id}")), ("Accept", "application/json")],
        &format!("multipart/form-data; boundary={BOUNDARY}"),
        &mut reader,
        body.len() as u64,
        MAX_BODY,
        Duration::from_secs(180),
    )
    .map_err(|e| (0, e))?;
    parse_upload(r.status, &r.body).map_err(|e| (r.status, e))
}

/// Uploads an image file's bytes. `configured_id` is `[uploads] imgur_client_id` (may be empty).
/// `total` receives the request size and `sent` counts up to it; setting `cancel` aborts.
pub fn upload(
    configured_id: &str,
    image: &[u8],
    file_name: &str,
    cancel: &AtomicBool,
    sent: &AtomicU64,
    total: &AtomicU64,
) -> Result<Uploaded, UploadError> {
    if image.len() as u64 > MAX_IMAGE_BYTES {
        return Err(UploadError::Failed(format!("{file_name} is larger than Imgur's 20 MB limit")));
    }
    let body = multipart(image, file_name);
    total.store(body.len() as u64, Ordering::Relaxed);
    let cancelled = || cancel.load(Ordering::Relaxed);
    let mut retried = false;
    loop {
        if cancelled() {
            return Err(UploadError::Cancelled);
        }
        let (id, scraped) = client_id(configured_id).map_err(UploadError::Failed)?;
        match upload_with(&id, &body, cancel, sent) {
            Ok((link, deletehash)) => return Ok(Uploaded { link, deletehash, client_id: id }),
            Err(_) if cancelled() => return Err(UploadError::Cancelled),
            // The website may have changed its ID since we read it: read it again, once, and
            // try again if it is a different one.
            Err((401 | 403 | 429, e)) if scraped && !retried => {
                retried = true;
                *SCRAPED.lock().unwrap_or_else(|e| e.into_inner()) = None;
                if client_id(configured_id).is_ok_and(|(new, _)| new == id) {
                    return Err(UploadError::Failed(e));
                }
            }
            Err((_, e)) => return Err(UploadError::Failed(e)),
        }
    }
}

/// Deletes an uploaded image again (an upload that was cancelled too late).
pub fn delete(uploaded: &Uploaded) -> Result<(), String> {
    if uploaded.deletehash.is_empty() || !uploaded.deletehash.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return Err("no delete hash".into());
    }
    let r = schwaetz_net::http::delete_with(
        &format!("{API}/image/{}", uploaded.deletehash),
        &[("Authorization", &format!("Client-ID {}", uploaded.client_id))],
        MAX_BODY,
    )?;
    if r.status == 200 { Ok(()) } else { Err(format!("Imgur answered HTTP {}", r.status)) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_id_from_the_website() {
        let html = r#"<script defer="defer" src="https://s.imgur.com/desktop-assets/js/main.2b1a65ae.js"></script>"#;
        assert_eq!(bundle_url(html).as_deref(), Some("https://s.imgur.com/desktop-assets/js/main.2b1a65ae.js"));
        assert_eq!(
            bundle_url("<script src='//s.imgur.com/desktop-assets/js/main.1.js'>").as_deref(),
            Some("https://s.imgur.com/desktop-assets/js/main.1.js")
        );
        assert_eq!(
            bundle_url("<script defer=defer src=https://s.imgur.com/desktop-assets/js/main.2b.js></script>").as_deref(),
            Some("https://s.imgur.com/desktop-assets/js/main.2b.js")
        );
        assert_eq!(bundle_url("<html></html>"), None);
        let js = r#"var r=!!{isProd:!0,environment:"production",apiClientId:"d70305e7c3ac5c6",version:"26485d4"}"#;
        assert_eq!(bundle_client_id(js).as_deref(), Some("d70305e7c3ac5c6"));
        assert_eq!(bundle_client_id(r#"apiClientId:"not hex!""#), None);
        assert_eq!(bundle_client_id("nothing here"), None);
    }

    #[test]
    fn configured_client_id_wins() {
        assert_eq!(client_id(" abc123 "), Ok(("abc123".to_owned(), false)));
    }

    #[test]
    fn multipart_body() {
        let body = multipart(b"\x89PNG", "my \"shot\".PNG");
        let text = String::from_utf8_lossy(&body);
        assert!(text.starts_with(&format!("--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"image\"; ")));
        assert!(text.contains("filename=\"my shot.PNG\"\r\nContent-Type: image/png\r\n\r\n\u{fffd}PNG\r\n"));
        assert!(text.ends_with(&format!("\r\n--{BOUNDARY}--\r\n")));
        assert!(is_image_name("a.JPeG") && !is_image_name("a.txt") && !is_image_name("png"));
    }

    #[test]
    fn upload_answers() {
        let ok = br#"{"data":{"id":"abc","deletehash":"dEl","link":"https://i.imgur.com/abc.png"},"success":true,"status":200}"#;
        assert_eq!(parse_upload(200, ok), Ok(("https://i.imgur.com/abc.png".to_owned(), "dEl".to_owned())));
        let err = br#"{"data":{"error":"Invalid client_id","request":"/3/image","method":"POST"},"success":false,"status":403}"#;
        assert_eq!(parse_upload(403, err), Err("Imgur: Invalid client_id (HTTP 403)".to_owned()));
        let nested =
            br#"{"data":{"error":{"code":1003,"message":"File type invalid (1)"}},"success":false,"status":400}"#;
        assert_eq!(parse_upload(400, nested), Err("Imgur: File type invalid (1) (HTTP 400)".to_owned()));
        assert_eq!(parse_upload(502, b"<html>"), Err("Imgur answered HTTP 502".to_owned()));
        let unknown =
            br#"{"errors":[{"id":"x","code":"429","status":"Too Many Requests","detail":"Too Many Requests"}]}"#;
        assert!(parse_upload(429, unknown).unwrap_err().contains("client ID"));
        let other = br#"{"errors":[{"code":"500","detail":"Something broke"}]}"#;
        assert_eq!(parse_upload(500, other), Err("Imgur: Something broke (HTTP 500)".to_owned()));
    }

    #[test]
    fn body_reader_counts_and_cancels() {
        let data = vec![7u8; 40_000];
        let (cancel, sent) = (AtomicBool::new(false), AtomicU64::new(0));
        let mut r = Body { data: &data, pos: 0, cancel: &cancel, sent: &sent };
        let mut buf = vec![0u8; 64 << 10];
        assert_eq!(r.read(&mut buf).unwrap(), 16 << 10);
        assert_eq!(sent.load(Ordering::Relaxed), 16 << 10);
        cancel.store(true, Ordering::Relaxed);
        assert!(r.read(&mut buf).is_err());
    }

    #[test]
    fn too_large_is_refused_before_any_request() {
        let big = vec![0u8; MAX_IMAGE_BYTES as usize + 1];
        let (cancel, sent, total) = (AtomicBool::new(false), AtomicU64::new(0), AtomicU64::new(0));
        assert!(matches!(upload("id", &big, "a.png", &cancel, &sent, &total), Err(UploadError::Failed(_))));
    }
}
