//! Library list queries: sort and page parameters, against a fake Jellyfin.

use jellydl::api::client::build_http;
use jellydl::api::{Jellyfin, Sort, SortField, identity};
use jellydl::config::{Overrides, parse};
use serde_json::json;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn children_sends_sort_and_page() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/Users/AuthenticateByName"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "AccessToken": "tok",
            "User": { "Id": "u1", "Name": "alice" }
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/Items"))
        .and(query_param("ParentId", "lib"))
        .and(query_param("SortBy", "DateCreated,SortName"))
        .and(query_param("SortOrder", "Descending"))
        .and(query_param("StartIndex", "300"))
        .and(query_param("Limit", "100"))
        .and(query_param("IncludeItemTypes", "Movie"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "Items": [{
                "Id": "m1", "Name": "Film", "Type": "Movie",
                "DateCreated": "2025-02-03T10:20:30.0000000Z",
                "PremiereDate": "1999-01-02T00:00:00.0000000Z",
                "CommunityRating": 7.4
            }],
            "TotalRecordCount": 1234
        })))
        .expect(1)
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let t = format!(
        "server_url = \"{}\"\n[auth]\nusername = \"alice\"\npassword = \"pw\"\n",
        server.uri()
    );
    let cfg = parse(&t, &Overrides::default()).unwrap();
    let http = build_http(&cfg).unwrap();
    let id = identity::Identity::new(dir.path(), &cfg.identity);
    let jf = Jellyfin::connect(&cfg, http, id, None).await.unwrap();

    let sort = Sort {
        field: SortField::DateAdded,
        descending: true,
    };
    let r = jf
        .children("lib", Some("movies"), sort, 300, 100)
        .await
        .unwrap();
    assert_eq!(r.total_record_count, 1234);
    let it = &r.items[0];
    assert_eq!(it.date_added(), Some("2025-02-03"));
    assert_eq!(it.release_date(), Some("1999-01-02"));
    assert_eq!(it.community_rating, Some(7.4));
}

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

fn people_reply() -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({
        "Id": "m1", "Name": "Film", "Type": "Movie",
        "People": [
            { "Name": "Ann Actor", "Role": "Hero", "Type": "Actor" },
            { "Name": "Dan Director", "Type": "Director" }
        ]
    }))
}

#[tokio::test]
async fn people_of_an_item() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let jf = login(&server, dir.path()).await;
    Mock::given(method("GET"))
        .and(path("/Items/m1"))
        .and(query_param("userId", "u1"))
        .and(query_param("Fields", "People"))
        .respond_with(people_reply())
        .expect(1)
        .mount(&server)
        .await;
    let p = jf.people("m1").await.unwrap();
    assert_eq!(p.len(), 2);
    assert_eq!(
        (p[0].name.as_str(), p[0].role.as_deref(), p[0].kind.as_str()),
        ("Ann Actor", Some("Hero"), "Actor")
    );
    assert_eq!(p[1].kind, "Director");
}

#[tokio::test]
async fn people_falls_back_to_the_legacy_route() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let jf = login(&server, dir.path()).await;
    Mock::given(method("GET"))
        .and(path("/Items/m1"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/Users/u1/Items/m1"))
        .respond_with(people_reply())
        .expect(1)
        .mount(&server)
        .await;
    assert_eq!(jf.people("m1").await.unwrap().len(), 2);
}

#[tokio::test]
async fn primary_image_returns_bytes_or_none() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let jf = login(&server, dir.path()).await;
    Mock::given(method("GET"))
        .and(path("/Items/m1/Images/Primary"))
        .and(query_param("maxHeight", "600"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![1u8, 2, 3]))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/Items/m2/Images/Primary"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    assert_eq!(
        jf.primary_image("m1", 600).await.unwrap(),
        Some(vec![1, 2, 3])
    );
    assert_eq!(jf.primary_image("m2", 600).await.unwrap(), None);
}
