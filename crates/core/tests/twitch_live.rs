//! Talks to Twitch's real OAuth server with the built-in client ID. Run manually:
//! `cargo test -p schwaetz-core --test twitch_live -- --ignored --nocapture`

use schwaetz_core::twitch_auth::{AuthRequest, AuthResponse, built_in_client_id, execute};

#[test]
#[ignore = "needs network access to id.twitch.tv"]
fn device_flow_and_refresh_endpoints_answer() {
    schwaetz_net::http::install_crypto();
    let client_id = built_in_client_id().expect("SCHWAETZ_TWITCH_CLIENT_ID is set in .cargo/config.toml").to_owned();

    let start = execute(&AuthRequest::StartDevice { client_id: client_id.clone() });
    let AuthResponse::Device { device_code, user_code, verification_uri, .. } = start else {
        panic!("no device code: {start:?}");
    };
    println!("user code {user_code}, page {verification_uri}");
    assert!(verification_uri.contains(&user_code));

    // Nobody authorizes this code, so Twitch keeps saying "pending".
    let poll = execute(&AuthRequest::PollDevice { client_id: client_id.clone(), device_code });
    println!("poll: {poll:?}");
    assert!(matches!(poll, AuthResponse::Pending | AuthResponse::SlowDown), "{poll:?}");

    let refresh = execute(&AuthRequest::Refresh { client_id, refresh_token: "not-a-real-token".into() });
    println!("refresh: {refresh:?}");
    assert!(matches!(refresh, AuthResponse::Invalid(_)), "{refresh:?}");
}
