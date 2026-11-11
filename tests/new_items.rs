//! "New since the last check" lists, against a fake Jellyfin.

use jellydl::api::client::build_http;
use jellydl::api::{Jellyfin, Sort, date, identity};
use jellydl::config::{Overrides, parse};
use jellydl::ui::new_items;
use jellydl::ui::view::Source;
use serde_json::{Value, json};
use wiremock::matchers::{method, path, query_param, query_param_is_missing};
use wiremock::{Mock, MockServer, ResponseTemplate};

const SINCE: &str = "2025-01-01T00:00:00Z";

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

fn since() -> i64 {
    date::parse_iso(SINCE).unwrap()
}

fn reply(items: Value, total: usize) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({ "Items": items, "TotalRecordCount": total }))
}

/// The newest-first list of files under `parent`.
async fn mount_media(server: &MockServer, parent: &str, items: Value, total: usize) {
    Mock::given(method("GET"))
        .and(path("/Items"))
        .and(query_param("ParentId", parent))
        .and(query_param(
            "IncludeItemTypes",
            "Movie,Episode,Video,MusicVideo",
        ))
        .and(query_param("SortBy", "DateCreated"))
        .and(query_param("SortOrder", "Descending"))
        .and(query_param("Recursive", "true"))
        .respond_with(reply(items, total))
        .expect(1)
        .mount(server)
        .await;
}

fn names(items: &[jellydl::api::BaseItem]) -> Vec<&str> {
    items.iter().map(|i| i.name.as_str()).collect()
}

#[tokio::test]
async fn new_movies_stop_at_the_last_check() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let jf = login(&server, dir.path()).await;
    // 500 files on the server, but the third one is older: one request only.
    mount_media(
        &server,
        "lib",
        json!([
            { "Id": "m2", "Name": "Zeta", "Type": "Movie", "DateCreated": "2025-03-01T00:00:00.0000000Z" },
            { "Id": "m1", "Name": "Alpha", "Type": "Movie", "DateCreated": "2025-02-01T00:00:00.0000000Z" },
            { "Id": "m0", "Name": "Old", "Type": "Movie", "DateCreated": "2024-12-31T23:59:59.0000000Z" }
        ]),
        500,
    )
    .await;
    let source = Source::Children {
        parent_id: "lib".into(),
        collection_type: Some("movies".into()),
    };
    let r = new_items::load(&jf, source, Sort::default(), since())
        .await
        .unwrap();
    assert_eq!(names(&r.items), ["Alpha", "Zeta"], "sorted by name");
    assert_eq!(r.total_record_count, 2);
}

#[tokio::test]
async fn a_series_with_a_new_episode_is_new() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let jf = login(&server, dir.path()).await;
    mount_media(
        &server,
        "shows",
        json!([
            { "Id": "e3", "Name": "Ep 3", "Type": "Episode", "SeriesId": "s2", "SeasonId": "s2a", "DateCreated": "2025-03-01T00:00:00Z" },
            { "Id": "e2", "Name": "Ep 2", "Type": "Episode", "SeriesId": "s1", "SeasonId": "s1b", "DateCreated": "2025-02-02T00:00:00Z" },
            { "Id": "e1", "Name": "Ep 1", "Type": "Episode", "SeriesId": "s1", "SeasonId": "s1b", "DateCreated": "2025-02-01T00:00:00Z" }
        ]),
        3,
    )
    .await;
    Mock::given(method("GET"))
        .and(path("/Items"))
        .and(query_param("Ids", "s1,s2"))
        .respond_with(reply(
            json!([
                { "Id": "s2", "Name": "Beta Show", "Type": "Series", "DateCreated": "2020-01-01T00:00:00Z" },
                { "Id": "s1", "Name": "Alpha Show", "Type": "Series", "DateCreated": "2019-01-01T00:00:00Z" }
            ]),
            2,
        ))
        .expect(1)
        .mount(&server)
        .await;
    let source = Source::Children {
        parent_id: "shows".into(),
        collection_type: Some("tvshows".into()),
    };
    let r = new_items::load(&jf, source, Sort::default(), since())
        .await
        .unwrap();
    assert_eq!(names(&r.items), ["Alpha Show", "Beta Show"]);
}

#[tokio::test]
async fn a_folder_with_a_new_file_deep_inside_is_new() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let jf = login(&server, dir.path()).await;
    // lib > f1 > f2 > v1 (new). f9 has nothing new, f8 was added itself.
    mount_media(
        &server,
        "lib",
        json!([
            { "Id": "v1", "Name": "Clip", "Type": "Video", "ParentId": "f2", "DateCreated": "2025-02-01T00:00:00Z" }
        ]),
        1,
    )
    .await;
    Mock::given(method("GET"))
        .and(path("/Items/f2/Ancestors"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "Id": "f1", "Name": "Holidays", "Type": "Folder" },
            { "Id": "root", "Name": "media", "Type": "Folder" }
        ])))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/Items"))
        .and(query_param("ParentId", "lib"))
        .and(query_param_is_missing("Recursive"))
        .respond_with(reply(
            json!([
                { "Id": "f1", "Name": "Holidays", "Type": "Folder", "DateCreated": "2020-01-01T00:00:00Z" },
                { "Id": "f8", "Name": "Party", "Type": "Folder", "DateCreated": "2025-01-05T00:00:00Z" },
                { "Id": "f9", "Name": "Work", "Type": "Folder", "DateCreated": "2020-01-01T00:00:00Z" }
            ]),
            3,
        ))
        .expect(1)
        .mount(&server)
        .await;
    let source = Source::Children {
        parent_id: "lib".into(),
        collection_type: Some("homevideos".into()),
    };
    let r = new_items::load(&jf, source, Sort::default(), since())
        .await
        .unwrap();
    assert_eq!(names(&r.items), ["Holidays", "Party"]);
}

#[tokio::test]
async fn seasons_and_episodes_are_filtered() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let jf = login(&server, dir.path()).await;
    mount_media(
        &server,
        "s1",
        json!([
            { "Id": "e5", "Name": "Ep 5", "Type": "Episode", "SeriesId": "s1", "SeasonId": "sea2", "DateCreated": "2025-02-01T00:00:00Z" }
        ]),
        1,
    )
    .await;
    Mock::given(method("GET"))
        .and(path("/Shows/s1/Seasons"))
        .respond_with(reply(
            json!([
                { "Id": "sea1", "Name": "Season 1", "Type": "Season", "DateCreated": "2020-01-01T00:00:00Z" },
                { "Id": "sea2", "Name": "Season 2", "Type": "Season", "DateCreated": "2020-01-01T00:00:00Z" }
            ]),
            2,
        ))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/Shows/s1/Episodes"))
        .respond_with(reply(
            json!([
                { "Id": "e4", "Name": "Ep 4", "Type": "Episode", "DateCreated": "2020-01-01T00:00:00Z" },
                { "Id": "e5", "Name": "Ep 5", "Type": "Episode", "DateCreated": "2025-02-01T00:00:00Z" }
            ]),
            2,
        ))
        .mount(&server)
        .await;
    let seasons = Source::Seasons {
        series_id: "s1".into(),
    };
    let r = new_items::load(&jf, seasons, Sort::default(), since())
        .await
        .unwrap();
    assert_eq!(names(&r.items), ["Season 2"]);

    let episodes = Source::Episodes {
        series_id: "s1".into(),
        season_id: "sea2".into(),
    };
    let r = new_items::load(&jf, episodes, Sort::default(), since())
        .await
        .unwrap();
    assert_eq!(names(&r.items), ["Ep 5"]);
}
