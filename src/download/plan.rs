//! Turn items into download jobs: skip what is already on disk or in the queue.

use std::collections::{HashMap, HashSet};

use super::NewJob;
use crate::api::BaseItem;
use crate::paths::{DownloadLayout, SeriesInfo, downloaded_path};

/// Jobs to add to the queue, and what was left out.
#[derive(Debug, Default)]
pub struct Plan {
    pub jobs: Vec<NewJob>,
    /// Items already downloaded, already in the queue, or given twice.
    pub skipped: usize,
    /// Sum of the known sizes of the jobs.
    pub size: u64,
    /// Jobs with an unknown size.
    pub unknown_size: usize,
}

/// Plan the download of `items`. Items that are not files are ignored.
/// `busy` holds the item ids that are in the queue. `series` gives the year and
/// status of the series of episodes, to choose their folder.
pub fn plan(
    items: Vec<BaseItem>,
    busy: &HashSet<&str>,
    layout: &DownloadLayout,
    series: &HashMap<String, SeriesInfo>,
    url: impl Fn(&str) -> String,
) -> Plan {
    let mut seen = HashSet::new();
    let mut p = Plan::default();
    for it in items.into_iter().filter(|i| i.is_playable()) {
        if busy.contains(it.id.as_str()) || !seen.insert(it.id.clone()) {
            p.skipped += 1;
            continue;
        }
        let info = it.series_id.as_ref().and_then(|id| series.get(id));
        if downloaded_path(&it, layout, info).is_some() {
            p.skipped += 1;
            continue;
        }
        let expected = it.size();
        match expected {
            Some(s) => p.size += s,
            None => p.unknown_size += 1,
        }
        p.jobs.push(NewJob {
            url: url(&it.id),
            title: it.label(),
            target: layout.target(&it, info),
            item_id: it.id,
            expected_size: expected,
        });
    }
    p
}
