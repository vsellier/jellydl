//! Web interface against a fake Jellyfin: list, download, follow the queue.

use std::time::Duration;

use jellydl::api::client::build_http;
use jellydl::api::{Jellyfin, identity};
use jellydl::config::{Overrides, parse};
use jellydl::download::{self, ManagerConfig};
use jellydl::paths::{DownloadLayout, Naming};
use serde_json::{Value, json};
use tokio::sync::mpsc;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

async fn login(server: &MockServer, dir: &std::path::Path) -> Jellyfin {
    Mock::given(method("POST"))
        .and(path("/Users/AuthenticateByName"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "AccessToken": "tok",
            "User": { "Id": "u1", "Name": "alice" }
        })))
        .mount(server)
        .await;
    let t = format!(
        "server_url = \"{}\"\n[auth]\nusername = \"alice\"\npassword = \"pw\"\n",
        server.uri()
    );
    let cfg = parse(&t, &Overrides::default()).unwrap();
    let http = build_http(&cfg).unwrap();
    let id = identity::Identity::new(dir, &cfg.identity);
    Jellyfin::connect(&cfg, http, id, None).await.unwrap()
}

fn reply(items: Value) -> ResponseTemplate {
    let n = items.as_array().map_or(0, Vec::len);
    ResponseTemplate::new(200).set_body_json(json!({ "Items": items, "TotalRecordCount": n }))
}

#[tokio::test]
async fn lists_and_downloads_with_the_layout() {
    let jf_server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let api = login(&jf_server, dir.path()).await;

    Mock::given(method("GET"))
        .and(path("/UserViews"))
        .respond_with(reply(json!([
            { "Id": "lib", "Name": "Films", "Type": "CollectionFolder", "CollectionType": "movies" }
        ])))
        .mount(&jf_server)
        .await;
    let movie = json!({
        "Id": "m1", "Name": "Wicked : Partie II", "Type": "Movie", "ProductionYear": 2025,
        "MediaSources": [{ "Size": 5, "Path": "/srv/films/Wicked Partie II 2025.mkv" }]
    });
    Mock::given(method("GET"))
        .and(path("/Items"))
        .and(query_param("ParentId", "lib"))
        .respond_with(reply(json!([movie.clone()])))
        .mount(&jf_server)
        .await;
    Mock::given(method("GET"))
        .and(path("/Items"))
        .and(query_param("Ids", "m1"))
        .respond_with(reply(json!([movie])))
        .mount(&jf_server)
        .await;
    Mock::given(method("GET"))
        .and(path("/Items/m1/Download"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"hello".to_vec()))
        .mount(&jf_server)
        .await;

    let root = dir.path().join("dl");
    let layout = DownloadLayout {
        default_dir: root.clone(),
        movies_dir: root.join("films"),
        series_ongoing_dir: root.join("series"),
        series_ended_dir: root.join("finies"),
        naming: Naming::Server,
    };
    let (mgr_tx, mgr_rx) = mpsc::unbounded_channel();
    let mgr = download::spawn(
        ManagerConfig {
            client: reqwest::Client::new(),
            auth_header: api.auth_header().to_string(),
            max_parallel: 2,
            queue_file: None,
            retry_base: Duration::from_millis(10),
            max_retries: 1,
            max_speed: None,
            start_delay: None,
        },
        mgr_tx,
    );
    let app = jellydl::web::app(api, mgr, mgr_rx, layout, 2);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let http = reqwest::Client::builder().no_proxy().build().unwrap();
    let get = |p: &str| {
        let (http, url) = (http.clone(), format!("{base}{p}"));
        async move {
            let r = http.get(url).send().await.unwrap();
            assert!(r.status().is_success(), "{}", r.status());
            r.json::<Value>().await.unwrap()
        }
    };

    // The page is served.
    let page = http.get(format!("{base}/")).send().await.unwrap();
    assert!(
        page.text()
            .await
            .unwrap()
            .contains("<title>jellydl</title>")
    );

    // First screen: the latest lists, then the libraries.
    let libs = get("/api/list?src=libraries").await;
    let names: Vec<&str> = libs["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        ["New movies", "New 4K movies", "New series", "Films"]
    );
    assert_eq!(libs["items"][0]["open"]["src"], "latest");
    assert_eq!(libs["items"][3]["open"]["ct"], "movies");

    // A library: the movie, with where it will go.
    let films = get("/api/list?src=children&id=lib&ct=movies&sort=date_added&desc=true").await;
    let m = &films["items"][0];
    assert_eq!(m["playable"], true);
    assert_eq!(m["downloaded"], Value::Null);
    let target = root.join("films/Wicked Partie II 2025.mkv");
    assert_eq!(m["target"], target.display().to_string());

    // Download it, then wait for the queue to finish.
    let r: Value = http
        .post(format!("{base}/api/download"))
        .json(&json!({ "ids": ["m1", "jellydl:latest:movies"] }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(r["queued"], 1);
    let mut done = false;
    for _ in 0..100 {
        let jobs = get("/api/jobs").await;
        if jobs["jobs"][0]["state"] == "Done" {
            done = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(done, "the download did not finish");
    assert_eq!(std::fs::read(&target).unwrap(), b"hello");

    // Now it is on disk, and a second download does nothing.
    let films = get("/api/list?src=children&id=lib&ct=movies").await;
    assert_eq!(
        films["items"][0]["downloaded"],
        target.display().to_string()
    );
    let r: Value = http
        .post(format!("{base}/api/download"))
        .json(&json!({ "ids": ["m1"] }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        (r["queued"].as_u64(), r["skipped"].as_u64()),
        (Some(0), Some(1))
    );

    // Bad requests get a clear error.
    let bad = http
        .get(format!("{base}/api/list?src=nope"))
        .send()
        .await
        .unwrap();
    assert_eq!(bad.status(), 400);
}
