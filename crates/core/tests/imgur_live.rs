//! Talks to the real Imgur. Run manually:
//! `cargo test -p schwaetz-core --test imgur_live -- --ignored --nocapture`
//!
//! The upload publishes an 8×8 image for a moment (it is deleted again), so it only runs with
//! `SCHWAETZ_IMGUR_UPLOAD_TEST=1`.

use schwaetz_core::imgur;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// An 8×8 grey PNG.
const PIXEL: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52, 0x00, 0x00, 0x00,
    0x08, 0x00, 0x00, 0x00, 0x08, 0x08, 0x02, 0x00, 0x00, 0x00, 0x4b, 0x6d, 0x29, 0xdc, 0x00, 0x00, 0x00, 0x0f, 0x49,
    0x44, 0x41, 0x54, 0x78, 0xda, 0x63, 0x68, 0xc0, 0x01, 0x18, 0x86, 0x96, 0x04, 0x00, 0x82, 0xf3, 0x60, 0x01, 0x25,
    0x95, 0xb3, 0x63, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
];

#[test]
#[ignore = "needs network access to imgur.com and publishes an image"]
fn uploads_and_deletes() {
    if std::env::var("SCHWAETZ_IMGUR_UPLOAD_TEST").as_deref() != Ok("1") {
        println!("set SCHWAETZ_IMGUR_UPLOAD_TEST=1 to upload");
        return;
    }
    schwaetz_net::http::install_crypto();
    let (cancel, sent, total) = (AtomicBool::new(false), AtomicU64::new(0), AtomicU64::new(0));
    let up = imgur::upload("", PIXEL, "pixel.png", &cancel, &sent, &total).expect("upload");
    println!("{up:?}");
    assert!(up.link.starts_with("https://i.imgur.com/") && up.link.ends_with(".png"), "{}", up.link);
    assert_eq!(sent.load(Ordering::Relaxed), total.load(Ordering::Relaxed));
    imgur::delete(&up).expect("delete");
}

#[test]
#[ignore = "needs network access to imgur.com"]
fn finds_the_websites_client_id() {
    schwaetz_net::http::install_crypto();
    let (id, scraped) = imgur::client_id("").expect("client id");
    println!("client id {id} (scraped: {scraped})");
    assert!(id.len() >= 8);
}

#[test]
fn cancelled_upload_sends_nothing() {
    schwaetz_net::http::install_crypto();
    let (cancel, sent, total) = (AtomicBool::new(true), AtomicU64::new(0), AtomicU64::new(0));
    let r = imgur::upload("", PIXEL, "pixel.png", &cancel, &sent, &total);
    assert_eq!(r, Err(imgur::UploadError::Cancelled));
}
