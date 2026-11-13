//! Items added to the server after a date (the "last check").
//!
//! Jellyfin cannot filter on the date an item was added. So the media files
//! (movies, episodes, videos) are listed newest first until one is older than
//! the date. A series, season or folder is new when it holds a new file.

use std::collections::HashSet;

use anyhow::Result;
use futures::{StreamExt, TryStreamExt, stream};

use super::client::LIST_FIELDS;
use super::models::LATEST_TYPE;
use super::{BaseItem, Jellyfin, QueryResult};

/// What the "latest additions" lists show.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LatestKind {
    Movies,
    Movies4k,
    Series,
}

impl LatestKind {
    pub const ALL: [LatestKind; 3] = [Self::Movies, Self::Movies4k, Self::Series];

    pub fn title(self) -> &'static str {
        match self {
            Self::Movies => "New movies",
            Self::Movies4k => "New 4K movies",
            Self::Series => "New series",
        }
    }

    fn id(self) -> &'static str {
        match self {
            Self::Movies => "jellydl:latest:movies",
            Self::Movies4k => "jellydl:latest:movies4k",
            Self::Series => "jellydl:latest:series",
        }
    }

    /// The virtual entry shown on the first screen.
    pub fn entry(self) -> BaseItem {
        BaseItem {
            id: self.id().into(),
            name: self.title().into(),
            item_type: LATEST_TYPE.into(),
            is_folder: Some(true),
            ..Default::default()
        }
    }

    pub fn from_entry(it: &BaseItem) -> Option<Self> {
        Self::ALL.into_iter().find(|k| it.id == k.id())
    }
}

/// Items in a latest additions list.
pub const LATEST_LIMIT: usize = 100;
/// Pages of movies read at most to find 4K ones.
const MAX_4K_PAGES: usize = 10;

/// The latest additions entries, to put before the libraries.
pub fn with_latest_entries(libs: Vec<BaseItem>) -> Vec<BaseItem> {
    LatestKind::ALL
        .into_iter()
        .map(LatestKind::entry)
        .chain(libs)
        .collect()
}

/// Item types that have a file. The others hold files.
const MEDIA_TYPES: &str = "Movie,Episode,Video,MusicVideo";
const PAGE: usize = 200;
/// Items asked by id in one request (the ids go in the URL).
const IDS_PER_REQUEST: usize = 40;

/// Which items are new since `since`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NewSet {
    /// Unix seconds (UTC).
    pub since: i64,
    /// New files, and the series, seasons and folders that hold them.
    ids: HashSet<String>,
}

impl NewSet {
    /// Only items with their own add date after `since` are new.
    pub fn empty(since: i64) -> Self {
        Self {
            since,
            ids: HashSet::new(),
        }
    }

    pub fn is_new(&self, it: &BaseItem) -> bool {
        self.ids.contains(&it.id) || it.added_after(self.since)
    }

    /// Mark these new files, with their series and season.
    fn add_media(&mut self, media: &[BaseItem]) {
        for m in media {
            self.ids.insert(m.id.clone());
            self.ids.extend(m.series_id.iter().cloned());
            self.ids.extend(m.season_id.iter().cloned());
        }
    }
}

impl Jellyfin {
    /// Files added after `since`, newest first. `scope` limits the search to the
    /// items under a library, folder or series.
    pub async fn new_media(&self, scope: Option<&str>, since: i64) -> Result<Vec<BaseItem>> {
        let fields = format!("{LIST_FIELDS},ParentId");
        let mut out = Vec::new();
        let mut start = 0;
        loop {
            let (s, l) = (start.to_string(), PAGE.to_string());
            let mut q = vec![
                ("userId", self.user_id.as_str()),
                ("Recursive", "true"),
                ("IncludeItemTypes", MEDIA_TYPES),
                ("SortBy", "DateCreated"),
                ("SortOrder", "Descending"),
                ("StartIndex", &s),
                ("Limit", &l),
                ("Fields", &fields),
                ("EnableImages", "false"),
            ];
            if let Some(p) = scope {
                q.push(("ParentId", p));
            }
            let r = self.get_items("/Items", &q).await?;
            let n = r.items.len();
            for it in r.items {
                if !it.added_after(since) {
                    return Ok(out);
                }
                out.push(it);
            }
            start += n;
            if n == 0 || start >= r.total_record_count {
                return Ok(out);
            }
        }
    }

    /// The new files under `scope`, and which items hold them.
    ///
    /// With `folders`, the folders above each file are found too (one request per
    /// series or folder that holds new files). Without it, only series and seasons.
    pub async fn new_set(
        &self,
        scope: Option<&str>,
        since: i64,
        folders: bool,
    ) -> Result<(Vec<BaseItem>, NewSet)> {
        let media = self.new_media(scope, since).await?;
        let mut set = NewSet::empty(since);
        set.add_media(&media);
        if folders {
            // Episodes: the folders above the series. Other files: above their folder.
            let anchors: HashSet<String> = media
                .iter()
                .filter_map(|m| m.series_id.clone().or_else(|| m.parent_id.clone()))
                .collect();
            set.ids.extend(anchors.iter().cloned());
            let above: Vec<Vec<BaseItem>> = stream::iter(anchors)
                .map(|id| async move { self.ancestors(&id).await })
                .buffer_unordered(4)
                .try_collect()
                .await?;
            set.ids.extend(above.into_iter().flatten().map(|it| it.id));
        }
        Ok((media, set))
    }

    /// The most recently added movies, 4K movies or series, newest first.
    pub async fn latest(&self, kind: LatestKind) -> Result<QueryResult> {
        let types = match kind {
            LatestKind::Series => "Series",
            _ => "Movie",
        };
        let (mut out, mut start) = (Vec::new(), 0);
        // 4K is not a server filter: read the newest movies and keep the 4K ones.
        let pages = if kind == LatestKind::Movies4k {
            MAX_4K_PAGES
        } else {
            1
        };
        for _ in 0..pages {
            let (s, l) = (start.to_string(), PAGE.to_string());
            let r = self
                .get_items(
                    "/Items",
                    &[
                        ("userId", self.user_id.as_str()),
                        ("Recursive", "true"),
                        ("IncludeItemTypes", types),
                        ("SortBy", "DateCreated"),
                        ("SortOrder", "Descending"),
                        ("StartIndex", &s),
                        ("Limit", &l),
                        ("Fields", LIST_FIELDS),
                        ("EnableImages", "false"),
                    ],
                )
                .await?;
            let n = r.items.len();
            out.extend(r.items.into_iter().filter(|it| match kind {
                LatestKind::Movies4k => it.is_4k(),
                _ => true,
            }));
            start += n;
            if n == 0 || start >= r.total_record_count || out.len() >= LATEST_LIMIT {
                break;
            }
        }
        out.truncate(LATEST_LIMIT);
        Ok(QueryResult {
            total_record_count: out.len(),
            items: out,
        })
    }

    /// Folders above an item, nearest first.
    pub async fn ancestors(&self, item_id: &str) -> Result<Vec<BaseItem>> {
        self.get(
            &format!("/Items/{item_id}/Ancestors"),
            &[("userId", &self.user_id)],
        )
        .await
    }

    /// Items by id, in no particular order.
    pub async fn items_by_ids(&self, ids: &[String]) -> Result<Vec<BaseItem>> {
        let mut out = Vec::with_capacity(ids.len());
        for chunk in ids.chunks(IDS_PER_REQUEST) {
            let ids = chunk.join(",");
            let r = self
                .get_items(
                    "/Items",
                    &[
                        ("userId", &self.user_id),
                        ("Ids", &ids),
                        ("Fields", LIST_FIELDS),
                        ("EnableImages", "false"),
                    ],
                )
                .await?;
            out.extend(r.items);
        }
        Ok(out)
    }

    /// Every child of a library or folder (all pages).
    pub async fn all_children(
        &self,
        parent_id: &str,
        collection_type: Option<&str>,
        sort: super::Sort,
    ) -> Result<Vec<BaseItem>> {
        const BIG_PAGE: usize = 500;
        let mut out = Vec::new();
        loop {
            let r = self
                .children(parent_id, collection_type, sort, out.len(), BIG_PAGE)
                .await?;
            let n = r.items.len();
            out.extend(r.items);
            if n == 0 || out.len() >= r.total_record_count {
                return Ok(out);
            }
        }
    }
}
