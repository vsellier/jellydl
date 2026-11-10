//! The shared HTTP client sends the configured User-Agent.

use jellydl::api::client::build_http;
use jellydl::config::{Overrides, parse};
use wiremock::matchers::{header, method};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn sends_configured_user_agent() {
    let server = MockServer::start().await;
    let ua = "Mozilla/5.0 (X11; Linux x86_64; rv:145.0) Gecko/20100101 Firefox/145.0";
    Mock::given(method("GET"))
        .and(header("user-agent", ua))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;

    let text = format!(
        "server_url = \"{}\"\nuser_agent = \"{ua}\"\n[auth]\napi_key = \"k\"\n",
        server.uri()
    );
    let cfg = parse(&text, &Overrides::default()).unwrap();
    let res = build_http(&cfg)
        .unwrap()
        .get(server.uri())
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
}
