//! Load a list with only the items that are new since the last check.
//!
//! The result is the whole filtered list in one page: new items are few, and a
//! filtered list cannot be paged on the server.

use std::cmp::Ordering;

use anyhow::Result;

use super::view::Source;
use crate::api::{BaseItem, Jellyfin, QueryResult, Sort, SortField};

pub async fn load(api: &Jellyfin, source: Source, sort: Sort, since: i64) -> Result<QueryResult> {
    let items = match source {
        // The first screen is never filtered.
        Source::Libraries => api.views().await?,
        Source::Children {
            parent_id,
            collection_type,
        } => {
            let mut items = match collection_type.as_deref() {
                // These libraries are shown flat: list the new files directly.
                Some("movies") => {
                    let media = api.new_media(Some(&parent_id), since).await?;
                    media
                        .into_iter()
                        .filter(|m| m.item_type == "Movie")
                        .collect()
                }
                Some("tvshows") => {
                    let media = api.new_media(Some(&parent_id), since).await?;
                    let mut ids: Vec<String> =
                        media.into_iter().filter_map(|m| m.series_id).collect();
                    ids.sort();
                    ids.dedup();
                    let mut series = api.items_by_ids(&ids).await?;
                    series.retain(|s| s.item_type == "Series");
                    series
                }
                _ => {
                    let (_, set) = api.new_set(Some(&parent_id), since, true).await?;
                    let mut all = api
                        .all_children(&parent_id, collection_type.as_deref(), sort)
                        .await?;
                    all.retain(|it| set.is_new(it));
                    all
                }
            };
            sort_items(&mut items, sort);
            items
        }
        Source::Seasons { series_id } => {
            let (_, set) = api.new_set(Some(&series_id), since, false).await?;
            let mut r = api.seasons(&series_id).await?;
            r.items.retain(|it| set.is_new(it));
            r.items
        }
        Source::Episodes {
            series_id,
            season_id,
        } => {
            let mut r = api.episodes(&series_id, &season_id).await?;
            r.items.retain(|it| it.added_after(since));
            r.items
        }
        Source::Search { term } => {
            let (_, set) = api.new_set(None, since, false).await?;
            let mut r = api.search(&term).await?;
            r.items.retain(|it| set.is_new(it));
            r.items
        }
    };
    Ok(QueryResult {
        total_record_count: items.len(),
        items,
    })
}

/// Sort like the server does for library lists. The name breaks ties.
pub fn sort_items(items: &mut [BaseItem], sort: Sort) {
    fn by<T: PartialOrd>(a: Option<T>, b: Option<T>) -> Ordering {
        // Missing values first, as for an ascending sort on the server.
        match (a, b) {
            (Some(a), Some(b)) => a.partial_cmp(&b).unwrap_or(Ordering::Equal),
            (a, b) => a.is_some().cmp(&b.is_some()),
        }
    }
    let release = |it: &BaseItem| {
        it.release_date()
            .map(str::to_string)
            .or_else(|| it.production_year.map(|y| format!("{y:04}")))
    };
    items.sort_by(|a, b| {
        let first = match sort.field {
            SortField::Name => Ordering::Equal,
            SortField::DateAdded => by(a.added_at(), b.added_at()),
            SortField::ReleaseDate => by(release(a), release(b)),
            SortField::Rating => by(a.community_rating, b.community_rating),
            SortField::Runtime => by(a.run_time_ticks, b.run_time_ticks),
        };
        let o = first.then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        if sort.descending { o.reverse() } else { o }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn it(name: &str, added: &str, rating: Option<f32>) -> BaseItem {
        BaseItem {
            id: name.into(),
            name: name.into(),
            date_created: Some(added.into()),
            community_rating: rating,
            ..Default::default()
        }
    }

    fn names(v: &[BaseItem]) -> Vec<&str> {
        v.iter().map(|i| i.name.as_str()).collect()
    }

    #[test]
    fn sorts_on_the_client() {
        let mut v = vec![
            it("b", "2024-01-02T00:00:00Z", Some(5.0)),
            it("A", "2024-01-03T00:00:00Z", None),
            it("c", "2024-01-01T00:00:00Z", Some(8.0)),
        ];
        sort_items(&mut v, Sort::default());
        assert_eq!(names(&v), ["A", "b", "c"]);
        let sort = |field, descending| Sort { field, descending };
        sort_items(&mut v, sort(SortField::DateAdded, true));
        assert_eq!(names(&v), ["A", "b", "c"]);
        sort_items(&mut v, sort(SortField::DateAdded, false));
        assert_eq!(names(&v), ["c", "b", "A"]);
        sort_items(&mut v, sort(SortField::Rating, true));
        assert_eq!(names(&v), ["c", "b", "A"], "no rating goes last");
    }
}
