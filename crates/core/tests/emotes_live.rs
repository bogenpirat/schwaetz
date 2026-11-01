//! Talks to the real 7TV and FFZ APIs. Run manually, optionally for another Twitch user id:
//! `SCHWAETZ_TWITCH_USER_ID=<id> cargo test -p schwaetz-core --test emotes_live -- --ignored --nocapture`

use schwaetz_core::emote_providers::Provider;

#[test]
#[ignore = "needs network access to 7tv.io and api.frankerfacez.com"]
fn personal_emotes_are_fetched() {
    schwaetz_net::http::install_crypto();
    // A channel with a 7TV personal set.
    let user_id = std::env::var("SCHWAETZ_TWITCH_USER_ID").unwrap_or_else(|_| "71092938".into());
    for provider in Provider::ALL.into_iter().filter(|p| p.has_personal_emotes()) {
        let emotes = provider.fetch_personal(&user_id).unwrap();
        println!("{}: {emotes:?}", provider.id());
        assert!(emotes.iter().all(|(name, url)| !name.is_empty() && url.starts_with("https://")));
        if provider == Provider::SevenTv {
            assert!(!emotes.is_empty(), "the 7TV personal set came empty");
        }
    }
}
