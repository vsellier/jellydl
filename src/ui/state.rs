//! UI choices kept between runs (`ui_state.json` in the data dir).

use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::api::Sort;

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct UiState {
    /// Last sort used in a library list.
    pub sort: Sort,
    /// Last check for new items, in Unix seconds. Items added after it are new.
    pub last_check: Option<i64>,
}

/// Load the saved state. A missing or bad file gives the defaults.
pub fn load(path: &Path) -> UiState {
    let Ok(data) = std::fs::read(path) else {
        return UiState::default();
    };
    serde_json::from_slice(&data).unwrap_or_else(|e| {
        tracing::warn!("ignoring bad {}: {e}", path.display());
        UiState::default()
    })
}

pub fn save(path: &Path, s: &UiState) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(s)?)
        .with_context(|| format!("cannot write {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("cannot write {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::SortField;

    #[test]
    fn round_trip_and_bad_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ui_state.json");
        assert_eq!(load(&path), UiState::default(), "no file");

        let s = UiState {
            sort: Sort {
                field: SortField::DateAdded,
                descending: true,
            },
            last_check: Some(1_700_000_000),
        };
        save(&path, &s).unwrap();
        assert_eq!(load(&path), s);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("\"date_added\""), "{text}");

        std::fs::write(&path, "{not json").unwrap();
        assert_eq!(load(&path), UiState::default());
        std::fs::write(&path, r#"{"sort":{"field":"gone"}}"#).unwrap();
        assert_eq!(load(&path), UiState::default(), "unknown field value");
    }
}
