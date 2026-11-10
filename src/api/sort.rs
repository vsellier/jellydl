//! Sort options for library lists, as in the Jellyfin web client.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SortField {
    #[default]
    Name,
    DateAdded,
    ReleaseDate,
    Rating,
    Runtime,
}

impl SortField {
    pub const ALL: [SortField; 5] = [
        SortField::Name,
        SortField::DateAdded,
        SortField::ReleaseDate,
        SortField::Rating,
        SortField::Runtime,
    ];

    pub fn label(self) -> &'static str {
        match self {
            SortField::Name => "Name",
            SortField::DateAdded => "Date added",
            SortField::ReleaseDate => "Release date",
            SortField::Rating => "Rating",
            SortField::Runtime => "Runtime",
        }
    }

    /// Value of the `SortBy` query parameter. The name breaks ties.
    pub fn query(self) -> &'static str {
        match self {
            SortField::Name => "SortName",
            SortField::DateAdded => "DateCreated,SortName",
            SortField::ReleaseDate => "PremiereDate,ProductionYear,SortName",
            SortField::Rating => "CommunityRating,SortName",
            SortField::Runtime => "Runtime,SortName",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Sort {
    pub field: SortField,
    pub descending: bool,
}

impl Sort {
    pub fn order_query(self) -> &'static str {
        if self.descending {
            "Descending"
        } else {
            "Ascending"
        }
    }

    /// "Name ↑" or "Date added ↓".
    pub fn label(self) -> String {
        let arrow = if self.descending { "↓" } else { "↑" };
        format!("{} {arrow}", self.field.label())
    }
}
