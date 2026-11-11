//! The subset of Jellyfin's JSON that jellydl uses. Unknown fields are ignored.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct BaseItem {
    pub id: String,
    pub name: String,
    #[serde(rename = "Type")]
    pub item_type: String,
    pub collection_type: Option<String>,
    pub is_folder: Option<bool>,
    pub production_year: Option<i32>,
    pub overview: Option<String>,
    pub run_time_ticks: Option<i64>,
    pub date_created: Option<String>,
    pub premiere_date: Option<String>,
    pub community_rating: Option<f32>,
    pub series_name: Option<String>,
    pub series_id: Option<String>,
    pub season_id: Option<String>,
    pub index_number: Option<i32>,
    pub parent_index_number: Option<i32>,
    pub child_count: Option<i32>,
    pub container: Option<String>,
    pub path: Option<String>,
    /// Folder that holds the item. Asked only when finding new items.
    pub parent_id: Option<String>,
    pub media_sources: Option<Vec<MediaSource>>,
    pub people: Option<Vec<Person>>,
}

/// An actor, director, writer... of an item.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct Person {
    pub name: String,
    pub role: Option<String>,
    /// "Actor", "GuestStar", "Director", "Writer", "Producer"...
    #[serde(rename = "Type")]
    pub kind: String,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct MediaSource {
    pub size: Option<u64>,
    pub container: Option<String>,
    pub path: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct QueryResult {
    pub items: Vec<BaseItem>,
    pub total_record_count: usize,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct AuthResult {
    pub access_token: String,
    pub user: User,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct User {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub policy: Option<Policy>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct Policy {
    pub is_administrator: bool,
    pub enable_content_downloading: Option<bool>,
}

impl BaseItem {
    /// True for items that can have a cast (actors, directors).
    pub fn has_people(&self) -> bool {
        matches!(
            self.item_type.as_str(),
            "Movie" | "Series" | "Season" | "Episode" | "Video" | "MusicVideo"
        )
    }

    /// True for a top-level library (Movies, Shows...).
    pub fn is_library(&self) -> bool {
        matches!(self.item_type.as_str(), "CollectionFolder" | "UserView")
    }

    /// True for items that have a file to download.
    pub fn is_playable(&self) -> bool {
        matches!(
            self.item_type.as_str(),
            "Movie" | "Episode" | "Video" | "MusicVideo"
        )
    }

    pub fn size(&self) -> Option<u64> {
        self.media_sources.as_ref()?.first()?.size
    }

    pub fn runtime_minutes(&self) -> Option<i64> {
        self.run_time_ticks.map(|t| t / 600_000_000)
    }

    /// Date part ("2024-03-01") of an ISO date such as "2024-03-01T12:00:00.0000000Z".
    pub fn date_added(&self) -> Option<&str> {
        Some(date_part(self.date_created.as_deref()?))
    }

    /// When the item was added to the server, in Unix seconds.
    pub fn added_at(&self) -> Option<i64> {
        super::date::parse_iso(self.date_created.as_deref()?)
    }

    /// True when the item was added to the server after `t` (Unix seconds).
    pub fn added_after(&self, t: i64) -> bool {
        self.added_at().is_some_and(|a| a > t)
    }

    pub fn release_date(&self) -> Option<&str> {
        Some(date_part(self.premiere_date.as_deref()?))
    }

    /// One-line label for lists.
    pub fn label(&self) -> String {
        match self.item_type.as_str() {
            "Episode" => {
                let ep = match (self.parent_index_number, self.index_number) {
                    (Some(s), Some(e)) => format!("S{s:02}E{e:02} "),
                    (None, Some(e)) => format!("E{e:02} "),
                    _ => String::new(),
                };
                match &self.series_name {
                    Some(series) => format!("{series} · {ep}{}", self.name),
                    None => format!("{ep}{}", self.name),
                }
            }
            _ => self.name_with_year(),
        }
    }

    /// "Name (Year)". The year is not added twice when the name already ends with it.
    pub fn name_with_year(&self) -> String {
        match self.production_year {
            Some(y) if !self.name.trim_end().ends_with(&format!("({y})")) => {
                format!("{} ({y})", self.name)
            }
            _ => self.name.clone(),
        }
    }

    /// Short tag shown before the label.
    pub fn kind_tag(&self) -> &'static str {
        match self.item_type.as_str() {
            "Movie" => "MOV",
            "Series" => "SER",
            "Season" => "SEA",
            "Episode" => "EP ",
            "BoxSet" => "SET",
            "CollectionFolder" | "UserView" => "LIB",
            "Video" | "MusicVideo" => "VID",
            _ => "DIR",
        }
    }
}

fn date_part(s: &str) -> &str {
    s.split('T').next().unwrap_or(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn libraries() {
        let item = |t: &str| BaseItem {
            item_type: t.into(),
            ..Default::default()
        };
        assert!(item("CollectionFolder").is_library());
        assert!(item("UserView").is_library());
        assert!(!item("Folder").is_library());
        assert!(!item("Movie").is_library());
    }
}
