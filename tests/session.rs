//! Login, session reuse and request headers, against a fake Jellyfin.

use jellydl::api::Jellyfin;
use jellydl::api::client::build_http;
use jellydl::api::identity;
use jellydl::config::{Overrides, parse};
use serde_json::json;
use wiremock::matchers::{header, header_regex, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const FF: &str = "Mozilla/5.0 (X11; Linux x86_64; rv:145.0) Gecko/20100101 Firefox/145.0";

fn login_reply(token: &str) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({
        "AccessToken": token,
        "User": { "Id": "u1", "Name": "alice" }
    }))
}

fn me_reply() -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({ "Id": "u1", "Name": "alice" }))
}

fn config(server: &MockServer, extra: &str) -> jellydl::config::Config {
    let t = format!(
        "server_url = \"{}\"\nuser_agent = \"{FF}\"\n{extra}\n[auth]\nusername = \"alice\"\npassword = \"pw\"\n",
        server.uri()
    );
    parse(&t, &Overrides::default()).unwrap()
}

#[tokio::test]
async fn reuses_saved_session_and_logs_in_again_when_invalid() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/Users/AuthenticateByName"))
        .respond_with(login_reply("tok1"))
        .up_to_n_times(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/Users/Me"))
        .and(header_regex("authorization", r#"Token="tok1""#))
        .respond_with(me_reply())
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let sess = dir.path().join("session.json");
    let cfg = config(&server, "");
    let http = build_http(&cfg).unwrap();
    let id = identity::Identity::new(dir.path(), &cfg.identity);

    let first = Jellyfin::connect(&cfg, http.clone(), id.clone(), Some(&sess))
        .await
        .unwrap();
    assert!(!first.reused_session);
    assert!(sess.exists());

    let second = Jellyfin::connect(&cfg, http.clone(), id.clone(), Some(&sess))
        .await
        .unwrap();
    assert!(second.reused_session, "the saved token must be used");
    assert_eq!(second.user_name, "alice");
    server.verify().await; // only one login so far

    // The token is revoked on the server: a new login happens.
    server.reset().await;
    Mock::given(method("GET"))
        .and(path("/Users/Me"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/Users/AuthenticateByName"))
        .respond_with(login_reply("tok2"))
        .expect(1)
        .mount(&server)
        .await;
    let third = Jellyfin::connect(&cfg, http, id, Some(&sess))
        .await
        .unwrap();
    assert!(!third.reused_session);
    assert!(third.auth_header().contains(r#"Token="tok2""#));
    assert_eq!(identity::load_session(&sess).unwrap().token, "tok2");
}

#[tokio::test]
async fn sends_jellydl_identity_and_extra_headers() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/Users/AuthenticateByName"))
        .and(header("user-agent", FF))
        .and(header("accept-language", "fr-FR"))
        .and(header("x-test", "1"))
        .and(header_regex(
            "authorization",
            r#"^MediaBrowser Client="jellydl", Device="jellydl", DeviceId="[0-9a-f]{32}", Version="[0-9.]+"$"#,
        ))
        .respond_with(login_reply("tok"))
        .expect(1)
        .mount(&server)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let cfg = config(
        &server,
        "[headers]\n\"Accept-Language\" = \"fr-FR\"\n\"X-Test\" = \"1\"",
    );
    let http = build_http(&cfg).unwrap();
    let id = identity::Identity::new(dir.path(), &cfg.identity);
    Jellyfin::connect(&cfg, http, id, None).await.unwrap();
}

#[tokio::test]
async fn sends_identity_overrides() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/Users/AuthenticateByName"))
        .and(header_regex(
            "authorization",
            r#"^MediaBrowser Client="myclient", Device="tower", DeviceId="dev-42", Version="9\.9"$"#,
        ))
        .respond_with(login_reply("tok"))
        .expect(1)
        .mount(&server)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let cfg = config(
        &server,
        "[identity]\nclient = \"myclient\"\ndevice = \"tower\"\ndevice_id = \"dev-42\"\nversion = \"9.9\"",
    );
    let http = build_http(&cfg).unwrap();
    let id = identity::Identity::new(dir.path(), &cfg.identity);
    Jellyfin::connect(&cfg, http, id, None).await.unwrap();
}
