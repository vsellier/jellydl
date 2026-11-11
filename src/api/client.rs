//! HTTP client for the Jellyfin REST API.

use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use reqwest::{Client, StatusCode, header};
use serde::de::DeserializeOwned;
use serde_json::json;

use super::identity::{self, Identity, Session};
use super::models::{AuthResult, BaseItem, Person, QueryResult, User};
use super::sort::Sort;
use crate::config::{Auth, Config};

pub(super) const LIST_FIELDS: &str =
    "MediaSources,ProductionYear,Overview,Path,ChildCount,DateCreated";

/// Build the shared reqwest client, with proxy, TLS options and extra headers.
pub fn build_http(cfg: &Config) -> Result<Client> {
    let mut headers = header::HeaderMap::new();
    for (k, v) in &cfg.headers {
        headers.insert(
            header::HeaderName::from_bytes(k.as_bytes())?,
            header::HeaderValue::from_str(v)?,
        );
    }
    let mut b = Client::builder()
        .user_agent(&cfg.user_agent)
        .default_headers(headers)
        .connect_timeout(Duration::from_secs(20))
        // Max time between two reads. A stalled download fails, then retries.
        .read_timeout(Duration::from_secs(60))
        .danger_accept_invalid_certs(cfg.accept_invalid_certs);
    if let Some(p) = &cfg.proxy {
        let mut proxy = reqwest::Proxy::all(p).with_context(|| "invalid proxy URL".to_string())?;
        if let Some(np) = &cfg.no_proxy {
            proxy = proxy.no_proxy(reqwest::NoProxy::from_string(np));
        }
        b = b.proxy(proxy);
    }
    // With no explicit proxy, reqwest reads HTTP(S)_PROXY / ALL_PROXY from the environment.
    Ok(b.build()?)
}

#[derive(Clone)]
pub struct Jellyfin {
    http: Client,
    base: String,
    auth: String,
    pub identity: Identity,
    pub user_id: String,
    pub user_name: String,
    /// True when a saved session was used (no new login on the server).
    pub reused_session: bool,
}

impl Jellyfin {
    /// Log in and return a ready client.
    ///
    /// With a username and password and a `session_file`, a saved token is tried first.
    /// A new login happens only when it is missing or no longer valid.
    pub async fn connect(
        cfg: &Config,
        http: Client,
        identity: Identity,
        session_file: Option<&Path>,
    ) -> Result<Self> {
        let base = cfg.server_url.clone();
        match &cfg.auth {
            Auth::Password { username, password } => {
                if let Some(file) = session_file
                    && let Some(s) = identity::load_session(file)
                {
                    let same = s.server_url == base
                        && &s.username == username
                        && s.device_id == identity.device_id;
                    if same {
                        let jf = Self {
                            http: http.clone(),
                            base: base.clone(),
                            auth: identity.header(Some(&s.token)),
                            identity: identity.clone(),
                            user_id: s.user_id.clone(),
                            user_name: s.user_name.clone(),
                            reused_session: true,
                        };
                        match jf.get::<User>("/Users/Me", &[]).await {
                            Ok(_) => {
                                tracing::info!("reusing the saved session");
                                return Ok(jf);
                            }
                            Err(e) => tracing::info!("saved session not valid ({e:#}); new login"),
                        }
                    }
                    identity::forget_session(file);
                }

                let res = http
                    .post(format!("{base}/Users/AuthenticateByName"))
                    .header(header::AUTHORIZATION, identity.header(None))
                    .json(&json!({ "Username": username, "Pw": password }))
                    .send()
                    .await
                    .with_context(|| format!("cannot reach {base}"))?;
                if res.status() == StatusCode::UNAUTHORIZED {
                    bail!("login failed: wrong username or password");
                }
                let a: AuthResult = check(res).await?.json().await.context("bad login reply")?;
                if let Some(file) = session_file {
                    let s = Session {
                        server_url: base.clone(),
                        username: username.clone(),
                        device_id: identity.device_id.clone(),
                        token: a.access_token.clone(),
                        user_id: a.user.id.clone(),
                        user_name: a.user.name.clone(),
                    };
                    if let Err(e) = identity::save_session(file, &s) {
                        tracing::warn!("cannot save the session: {e}");
                    }
                }
                Ok(Self {
                    http,
                    base,
                    auth: identity.header(Some(&a.access_token)),
                    identity,
                    user_id: a.user.id,
                    user_name: a.user.name,
                    reused_session: false,
                })
            }
            Auth::ApiKey(key) => {
                let mut jf = Self {
                    http,
                    base,
                    auth: identity.header(Some(key)),
                    identity,
                    user_id: String::new(),
                    user_name: String::new(),
                    reused_session: false,
                };
                // An API key has no user. /Users/Me fails, so pick a user from the list.
                let user = match jf.get::<User>("/Users/Me", &[]).await {
                    Ok(u) => u,
                    Err(_) => {
                        let users: Vec<User> = jf
                            .get("/Users", &[])
                            .await
                            .context("cannot list users with this API key")?;
                        let u = users
                            .iter()
                            .find(|u| u.policy.as_ref().is_some_and(|p| p.is_administrator))
                            .or(users.first())
                            .cloned()
                            .context("the server has no users")?;
                        tracing::warn!("API key: browsing as user {}", u.name);
                        u
                    }
                };
                jf.user_id = user.id;
                jf.user_name = user.name;
                Ok(jf)
            }
        }
    }

    pub fn http(&self) -> &Client {
        &self.http
    }

    pub fn auth_header(&self) -> &str {
        &self.auth
    }

    pub fn base_url(&self) -> &str {
        &self.base
    }

    pub fn download_url(&self, item_id: &str) -> String {
        format!("{}/Items/{item_id}/Download", self.base)
    }

    pub(super) async fn get_items(
        &self,
        path: &str,
        query: &[(&str, &str)],
    ) -> Result<QueryResult> {
        self.get(path, query).await
    }

    pub(super) async fn get<T: DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, &str)],
    ) -> Result<T> {
        let res = self
            .http
            .get(format!("{}{path}", self.base))
            .header(header::AUTHORIZATION, &self.auth)
            .query(query)
            .send()
            .await
            .with_context(|| format!("GET {path} failed"))?;
        check(res)
            .await?
            .json()
            .await
            .with_context(|| format!("GET {path}: bad JSON"))
    }

    /// Top-level libraries of the user.
    pub async fn views(&self) -> Result<Vec<BaseItem>> {
        let uid = self.user_id.clone();
        let r: QueryResult = match self.get("/UserViews", &[("userId", &uid)]).await {
            Ok(r) => r,
            // Older servers only have the legacy route.
            Err(_) => self.get(&format!("/Users/{uid}/Views"), &[]).await?,
        };
        Ok(r.items
            .into_iter()
            .filter(|v| {
                matches!(
                    v.collection_type.as_deref(),
                    None | Some("movies")
                        | Some("tvshows")
                        | Some("mixed")
                        | Some("homevideos")
                        | Some("boxsets")
                        | Some("folders")
                )
            })
            .collect())
    }

    /// Children of a library or folder, one page at a time.
    pub async fn children(
        &self,
        parent_id: &str,
        collection_type: Option<&str>,
        sort: Sort,
        start: usize,
        limit: usize,
    ) -> Result<QueryResult> {
        let start = start.to_string();
        let limit = limit.to_string();
        let mut q: Vec<(&str, &str)> = vec![
            ("userId", &self.user_id),
            ("ParentId", parent_id),
            ("SortBy", sort.field.query()),
            ("SortOrder", sort.order_query()),
            ("StartIndex", &start),
            ("Limit", &limit),
            ("Fields", LIST_FIELDS),
            ("EnableImages", "false"),
        ];
        // Show movie and series libraries flat, as the web client does.
        match collection_type {
            Some("movies") => q.extend([("Recursive", "true"), ("IncludeItemTypes", "Movie")]),
            Some("tvshows") => q.extend([("Recursive", "true"), ("IncludeItemTypes", "Series")]),
            _ => {}
        }
        self.get("/Items", &q).await
    }

    pub async fn seasons(&self, series_id: &str) -> Result<QueryResult> {
        self.get(
            &format!("/Shows/{series_id}/Seasons"),
            &[("userId", &self.user_id), ("Fields", LIST_FIELDS)],
        )
        .await
    }

    pub async fn episodes(&self, series_id: &str, season_id: &str) -> Result<QueryResult> {
        self.get(
            &format!("/Shows/{series_id}/Episodes"),
            &[
                ("userId", &self.user_id),
                ("SeasonId", season_id),
                ("Fields", LIST_FIELDS),
            ],
        )
        .await
    }

    pub async fn search(&self, term: &str) -> Result<QueryResult> {
        self.get(
            "/Items",
            &[
                ("userId", &self.user_id),
                ("SearchTerm", term),
                ("Recursive", "true"),
                ("IncludeItemTypes", "Movie,Series,Episode"),
                ("Limit", "100"),
                ("Fields", LIST_FIELDS),
                ("EnableImages", "false"),
            ],
        )
        .await
    }

    /// Primary image (poster) of an item, at most `max_height` pixels high.
    /// `None` when the item has no image.
    pub async fn primary_image(&self, item_id: &str, max_height: u32) -> Result<Option<Vec<u8>>> {
        const MAX_BYTES: u64 = 10 * 1024 * 1024;
        let path = format!("/Items/{item_id}/Images/Primary");
        let res = self
            .http
            .get(format!("{}{path}", self.base))
            .header(header::AUTHORIZATION, &self.auth)
            .query(&[
                ("maxHeight", max_height.to_string()),
                ("quality", "90".into()),
            ])
            .send()
            .await
            .with_context(|| format!("GET {path} failed"))?;
        if res.status() == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        let res = check(res).await?;
        if res.content_length().is_some_and(|n| n > MAX_BYTES) {
            bail!("GET {path}: image too large");
        }
        let bytes = res
            .bytes()
            .await
            .with_context(|| format!("GET {path}: read failed"))?;
        if bytes.len() as u64 > MAX_BYTES {
            bail!("GET {path}: image too large");
        }
        Ok(Some(bytes.to_vec()))
    }

    /// Cast and crew of one item.
    pub async fn people(&self, item_id: &str) -> Result<Vec<Person>> {
        let uid = self.user_id.clone();
        let q = [("userId", uid.as_str()), ("Fields", "People")];
        let item: BaseItem = match self.get(&format!("/Items/{item_id}"), &q).await {
            Ok(i) => i,
            // Older servers only have the legacy route.
            Err(_) => {
                self.get(&format!("/Users/{uid}/Items/{item_id}"), &q[1..])
                    .await?
            }
        };
        Ok(item.people.unwrap_or_default())
    }

    /// All downloadable items under a folder, series or season.
    pub async fn descendants(&self, parent_id: &str) -> Result<Vec<BaseItem>> {
        let r: QueryResult = self
            .get(
                "/Items",
                &[
                    ("userId", &self.user_id),
                    ("ParentId", parent_id),
                    ("Recursive", "true"),
                    ("IncludeItemTypes", "Movie,Episode,Video,MusicVideo"),
                    (
                        "SortBy",
                        "SeriesSortName,ParentIndexNumber,IndexNumber,SortName",
                    ),
                    ("Fields", LIST_FIELDS),
                    ("EnableImages", "false"),
                ],
            )
            .await?;
        Ok(r.items)
    }
}

async fn check(res: reqwest::Response) -> Result<reqwest::Response> {
    let status = res.status();
    if status.is_success() {
        return Ok(res);
    }
    let body = res.text().await.unwrap_or_default();
    let body: String = body.chars().take(200).collect();
    bail!("HTTP {status}: {body}")
}
