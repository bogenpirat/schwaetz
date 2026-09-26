//! Decoding real-world files (network access; run with `cargo test -- --ignored`).

fn fetch_decode(url: &str) -> (u32, u32, usize) {
    schwaetz_net::http::install_crypto();
    let r = schwaetz_net::http::get(url, 20 << 20, false).expect("download");
    let (w, h, frames) = schwaetz_media::decode_frames(&r.body, 112).unwrap_or_else(|e| panic!("{url}: {e}"));
    (w, h, frames.len())
}

#[test]
#[ignore]
fn animated_gif_and_webp() {
    let (_, _, n) = fetch_decode("https://upload.wikimedia.org/wikipedia/commons/2/2c/Rotating_earth_%28large%29.gif");
    assert!(n > 10, "gif frames: {n}");
    let (w, h, n) = fetch_decode("https://cdn.7tv.app/emote/60ae958e229664e8667aea38/2x.webp");
    eprintln!("7tv webp {w}x{h} frames={n}");
    let (w, h, n) = fetch_decode("https://static-cdn.jtvnw.net/emoticons/v2/25/default/dark/2.0");
    eprintln!("twitch png {w}x{h} frames={n}");
}
