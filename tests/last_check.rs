//! The saved last check: created at the first start, moved only with `N`.

use std::time::Duration;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use jellydl::api::client::build_http;
use jellydl::api::{Jellyfin, date, identity};
use jellydl::config::{Overrides, parse};
use jellydl::download::{self, ManagerConfig};
use jellydl::ui::app::App;
use jellydl::ui::state;
use serde_json::json;
use tokio::sync::mpsc;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

async fn app(server: &MockServer, dir: &std::path::Path, state_file: std::path::PathBuf) -> App {
    Mock::given(method("POST"))
        .and(path("/Users/AuthenticateByName"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "AccessToken": "tok",
            "User": { "Id": "u1", "Name": "alice" }
        })))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({ "Items": [], "TotalRecordCount": 0 })),
        )
        .mount(server)
        .await;
    let t = format!(
        "server_url = \"{}\"\n[auth]\nusername = \"alice\"\npassword = \"pw\"\n",
        server.uri()
    );
    let cfg = parse(&t, &Overrides::default()).unwrap();
    let http = build_http(&cfg).unwrap();
    let id = identity::Identity::new(dir, &cfg.identity);
    let jf = Jellyfin::connect(&cfg, http.clone(), id, None)
        .await
        .unwrap();
    let (mgr_tx, _mgr_rx) = mpsc::unbounded_channel();
    let mgr = download::spawn(
        ManagerConfig {
            client: http,
            auth_header: String::new(),
            max_parallel: 1,
            queue_file: None,
            retry_base: Duration::from_millis(10),
            max_retries: 0,
            max_speed: None,
            start_delay: None,
        },
        mgr_tx,
    );
    let (tx, _rx) = mpsc::unbounded_channel();
    App::new(
        jf,
        tx,
        mgr,
        jellydl::paths::DownloadLayout::single(dir.join("dl")),
        None,
        1,
        None,
    )
    .with_state_file(state_file)
}

fn key(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
}

#[tokio::test]
async fn first_start_saves_now_and_n_resets_it() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("ui_state.json");

    let before = date::now();
    let mut a = app(&server, dir.path(), file.clone()).await;
    let saved = state::load(&file)
        .last_check
        .expect("created at the first start");
    assert!(saved >= before && saved <= date::now());
    assert_eq!(a.last_check, saved);
    assert!(!a.new_only, "the filter is off at start");

    // A saved last check is kept as it is.
    let old = saved - 30 * 86_400;
    state::save(
        &file,
        &state::UiState {
            last_check: Some(old),
            ..Default::default()
        },
    )
    .unwrap();
    let mut a2 = app(&server, dir.path(), file.clone()).await;
    assert_eq!(a2.last_check, old);

    // N asks first; any key but y cancels.
    a2.on_key(key('N'));
    a2.on_key(key('x'));
    assert_eq!(a2.last_check, old);
    a2.on_key(key('N'));
    a2.on_key(key('y'));
    assert!(a2.last_check >= saved);
    assert_eq!(state::load(&file).last_check, Some(a2.last_check));

    // n only toggles the filter: the last check does not move.
    let t = a.last_check;
    a.on_key(key('n'));
    assert!(a.new_only);
    a.on_key(key('n'));
    assert!(!a.new_only);
    assert_eq!(a.last_check, t);
}
