//! Minimal blocking HTTPS client for link previews, emote images, scripts and the Twitch API.
//!
//! Uses the same rustls/ring stack and Windows certificate store as IRC connections. Call from
//! worker threads only.

use std::io::Read;
use std::time::Duration;
use ureq::ResponseExt;

pub struct Response {
    pub status: u16,
    pub content_type: String,
    pub body: Vec<u8>,
    /// Final URL after redirects.
    pub url: String,
}

/// Installs the process-wide rustls crypto provider (idempotent).
pub fn install_crypto() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

fn agent() -> ureq::Agent {
    use ureq::tls::{RootCerts, TlsConfig, TlsProvider};
    install_crypto();
    let tls = TlsConfig::builder().provider(TlsProvider::Rustls).root_certs(RootCerts::PlatformVerifier).build();
    ureq::Agent::config_builder()
        .tls_config(tls)
        .timeout_global(Some(Duration::from_secs(15)))
        .max_redirects(5)
        .user_agent(concat!("schwaetz/", env!("CARGO_PKG_VERSION"), " (link preview)"))
        .http_status_as_error(false)
        .build()
        .into()
}

/// GETs an `https://` (or `http://` if `allow_http`) URL, reading at most `max_bytes`.
pub fn get(url: &str, max_bytes: u64, allow_http: bool) -> Result<Response, String> {
    get_with(url, &[], max_bytes, allow_http)
}

/// Like [`get`], with extra request headers (API tokens and the like).
pub fn get_with(url: &str, headers: &[(&str, &str)], max_bytes: u64, allow_http: bool) -> Result<Response, String> {
    let lower = url.to_ascii_lowercase();
    if !(lower.starts_with("https://") || allow_http && lower.starts_with("http://")) {
        return Err("only https URLs are fetched".into());
    }
    let mut req = agent().get(url).header("Accept-Encoding", "gzip");
    for (k, v) in headers {
        req = req.header(*k, *v);
    }
    let resp = req.call().map_err(|e| e.to_string())?;
    let status = resp.status().as_u16();
    let content_type =
        resp.headers().get("content-type").and_then(|v| v.to_str().ok()).unwrap_or("").to_ascii_lowercase();
    let final_url = resp.get_uri().to_string();
    let mut body = Vec::new();
    resp.into_body().into_reader().take(max_bytes + 1).read_to_end(&mut body).map_err(|e| e.to_string())?;
    if body.len() as u64 > max_bytes {
        return Err(format!("response larger than {max_bytes} bytes"));
    }
    Ok(Response { status, content_type, body, url: final_url })
}
