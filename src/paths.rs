//! Where a downloaded item goes on disk.
//!
//! Names use only letters, digits, dashes and dots.
//! Jellyfin and Plex read the title, year and episode
//! number from this style, so the folder can be added
//! back to a media server as is.
//!
//! Older versions saved files in other places. [`older_targets_for`] gives
//! them, so these files are still found:
//! - `Movies/Night Harbor Return (2024)/...`: names with spaces, in a folder by type.
//! - `Movies/Night.Harbor.Return.2024/...`: names with dots, in a folder by type.

use std::path::{Path, PathBuf};

use crate::api::BaseItem;

const MAX_NAME_BYTES: usize = 200;

/// Separator that replaces spaces and special characters.
const SEP: char = '.';

/// Plain ASCII for a Latin letter with an accent (`é` gives `e`, `ß` gives `ss`).
/// `None` for other characters: non-Latin letters (東京) are kept as they are.
fn plain_latin(c: char) -> Option<&'static str> {
    let latin = matches!(c, '\u{00C0}'..='\u{024F}' | '\u{1E00}'..='\u{1EFF}');
    if !latin || !c.is_alphabetic() {
        return None;
    }
    deunicode::deunicode_char(c)
        .filter(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_alphanumeric()))
}

/// One path component with only letters, digits, dashes and dots:
/// "Élodie (2001)" gives "Elodie.2001", "Anna's Garden" gives "Annas.Garden".
/// Letters with no plain version (東京) are kept, so the name is never empty.
pub fn clean_name(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut sep = false;
    for c in s.chars() {
        let piece = match c {
            // Apostrophes join the word: "Anna's" gives "Annas".
            '\'' | '’' | '‘' | 'ʼ' | '`' => continue,
            '-' => Some("-"),
            c if c.is_ascii_alphanumeric() => None,
            c => match plain_latin(c) {
                Some(p) => Some(p),
                None if c.is_alphanumeric() => None,
                None => {
                    sep = true;
                    continue;
                }
            },
        };
        if sep && !out.is_empty() {
            out.push(SEP);
        }
        sep = false;
        match piece {
            Some(p) => out.push_str(p),
            None => out.push(c),
        }
    }
    let mut out = truncate_bytes(&out, MAX_NAME_BYTES).to_string();
    while out.ends_with([SEP, '-']) {
        out.pop();
    }
    while out.starts_with('-') {
        out.remove(0);
    }
    if out.is_empty() { "_".to_string() } else { out }
}

/// Path component as older versions made it: spaces kept, only characters that
/// are invalid on Windows removed. Used to find files downloaded before.
pub fn legacy_sanitize(s: &str) -> String {
    let cleaned: String = s
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => ' ',
            c if c.is_control() => ' ',
            c => c,
        })
        .collect();
    // Collapse runs of spaces.
    let mut out = String::with_capacity(cleaned.len());
    for w in cleaned.split_whitespace() {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(w);
    }
    let mut out = truncate_bytes(&out, MAX_NAME_BYTES).to_string();
    while out.ends_with(['.', ' ']) {
        out.pop();
    }
    if out.is_empty() { "_".to_string() } else { out }
}

fn truncate_bytes(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// File extension of the original media file.
pub fn extension(item: &BaseItem) -> String {
    let src = item.media_sources.as_ref().and_then(|m| m.first());
    let from_path = src
        .and_then(|s| s.path.as_deref())
        .or(item.path.as_deref())
        .and_then(|p| Path::new(p).extension())
        .and_then(|e| e.to_str())
        .filter(|e| e.len() <= 5 && e.chars().all(|c| c.is_ascii_alphanumeric()));
    if let Some(e) = from_path {
        return e.to_ascii_lowercase();
    }
    // "Container" can be a list such as "mov,mp4,m4a"; take the first one.
    src.and_then(|s| s.container.as_deref())
        .or(item.container.as_deref())
        .and_then(|c| c.split(',').next())
        .map(|c| c.trim().to_ascii_lowercase())
        .filter(|c| !c.is_empty())
        .unwrap_or_else(|| "mkv".to_string())
}

/// Where `item` is saved: `Night.Harbor.Return.2024/Night.Harbor.Return.2024.mkv`, or
/// `Garden.Diaries/S01/Garden.Diaries.S01E03.Pilot.mkv`.
pub fn target_for(item: &BaseItem, root: &Path) -> PathBuf {
    root.join(dotted_path(item, "Unknown"))
}

/// Path of `item` with dotted names, relative to the download folder.
/// `unknown` is the folder name when an episode has no series name.
fn dotted_path(item: &BaseItem, unknown: &str) -> PathBuf {
    let ext = extension(item);
    match item.item_type.as_str() {
        "Episode" => {
            let series = clean_name(item.series_name.as_deref().unwrap_or(unknown));
            let season = item.parent_index_number.unwrap_or(0);
            let ep = match item.index_number {
                Some(e) => format!("S{season:02}E{e:02}"),
                None => format!("S{season:02}"),
            };
            let file = clean_name(&format!("{series} {ep} {}", item.name));
            Path::new(&series)
                .join(format!("S{season:02}"))
                .join(format!("{file}.{ext}"))
        }
        "Movie" => {
            let name = clean_name(&item.name_with_year());
            Path::new(&name).join(format!("{name}.{ext}"))
        }
        _ => PathBuf::from(format!("{}.{ext}", clean_name(&item.name))),
    }
}

/// Folder by type that older versions put first.
fn type_folder(item: &BaseItem) -> &'static str {
    match item.item_type.as_str() {
        "Episode" => "Shows",
        "Movie" => "Movies",
        _ => "Other",
    }
}

/// Where older versions saved `item`, newest first.
pub fn older_targets_for(item: &BaseItem, root: &Path) -> [PathBuf; 2] {
    [
        root.join(type_folder(item))
            .join(dotted_path(item, "Unknown series")),
        legacy_target_for(item, root),
    ]
}

/// Where the first versions saved `item` (names with spaces).
fn legacy_target_for(item: &BaseItem, root: &Path) -> PathBuf {
    let sanitize = legacy_sanitize;
    let ext = extension(item);
    match item.item_type.as_str() {
        "Episode" => {
            let series = sanitize(item.series_name.as_deref().unwrap_or("Unknown series"));
            let season = item.parent_index_number.unwrap_or(0);
            let ep = match item.index_number {
                Some(e) => format!("S{season:02}E{e:02}"),
                None => format!("S{season:02}"),
            };
            let file = sanitize(&format!("{series} - {ep} - {}", item.name));
            root.join("Shows")
                .join(&series)
                .join(format!("Season {season:02}"))
                .join(format!("{file}.{ext}"))
        }
        "Movie" => {
            let name = sanitize(&item.name_with_year());
            root.join("Movies")
                .join(&name)
                .join(format!("{name}.{ext}"))
        }
        _ => {
            let name = sanitize(&item.name);
            root.join("Other").join(format!("{name}.{ext}"))
        }
    }
}

/// How files and series folders are named.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Naming {
    /// Letters, digits, dashes and dots only: `Garden.Diaries.S01E03.Pilot.mkv`.
    #[default]
    Clean,
    /// The file name on the server, and `Series (Year)` folders.
    Server,
}

impl Naming {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "clean" => Some(Self::Clean),
            "server" => Some(Self::Server),
            _ => None,
        }
    }
}

/// What jellydl needs to know about a series to place its episodes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SeriesInfo {
    pub year: Option<i32>,
    /// `Some(true)`: no new episode will come. `None`: the server does not say.
    pub ended: Option<bool>,
}

impl SeriesInfo {
    pub fn from_item(it: &BaseItem) -> Self {
        Self {
            year: it.production_year,
            ended: it
                .status
                .as_deref()
                .map(|s| s.eq_ignore_ascii_case("ended")),
        }
    }
}

/// The folders where downloads go, and how they are named.
#[derive(Debug, Clone)]
pub struct DownloadLayout {
    /// Other videos, and everything that has no folder of its own.
    pub default_dir: PathBuf,
    pub movies_dir: PathBuf,
    pub series_ongoing_dir: PathBuf,
    pub series_ended_dir: PathBuf,
    pub naming: Naming,
}

impl DownloadLayout {
    /// One folder for everything.
    pub fn single(dir: PathBuf) -> Self {
        Self {
            movies_dir: dir.clone(),
            series_ongoing_dir: dir.clone(),
            series_ended_dir: dir.clone(),
            default_dir: dir,
            naming: Naming::Clean,
        }
    }

    /// Folder (and name) a series is saved in, under its root.
    fn series_folder(&self, it: &BaseItem, naming: Naming, info: Option<&SeriesInfo>) -> String {
        let name = it.series_name.as_deref().unwrap_or("Unknown");
        match naming {
            Naming::Clean => clean_name(name),
            Naming::Server => {
                let base = legacy_sanitize(name);
                match info.and_then(|i| i.year) {
                    Some(y) if !base.ends_with(&format!("({y})")) => format!("{base} ({y})"),
                    _ => base,
                }
            }
        }
    }

    /// Name of the file, from the server or built from the metadata.
    fn file_name(&self, it: &BaseItem, naming: Naming) -> String {
        let ext = extension(it);
        if naming == Naming::Server {
            let src = it.media_sources.as_ref().and_then(|m| m.first());
            let from_server = src
                .and_then(|s| s.path.as_deref())
                .or(it.path.as_deref())
                .and_then(|p| p.rsplit(['/', '\\']).next())
                .and_then(|f| Path::new(f).file_stem())
                .and_then(|f| f.to_str())
                .map(legacy_sanitize)
                .filter(|f| f != "_");
            if let Some(stem) = from_server {
                return format!("{stem}.{ext}");
            }
            let fallback = match it.item_type.as_str() {
                "Episode" => legacy_sanitize(&it.label().replace(" · ", " ")),
                _ => legacy_sanitize(&it.name_with_year()),
            };
            return format!("{fallback}.{ext}");
        }
        dotted_path(it, "Unknown")
            .file_name()
            .map(|f| f.to_string_lossy().into_owned())
            .unwrap_or_default()
    }

    /// Path of `it` under its root, for one naming.
    fn relative(&self, it: &BaseItem, naming: Naming, info: Option<&SeriesInfo>) -> PathBuf {
        match it.item_type.as_str() {
            "Episode" => {
                let season = it.parent_index_number.unwrap_or(0);
                Path::new(&self.series_folder(it, naming, info))
                    .join(format!("S{season:02}"))
                    .join(self.file_name(it, naming))
            }
            "Movie" => match naming {
                Naming::Clean => dotted_path(it, "Unknown"),
                Naming::Server => PathBuf::from(self.file_name(it, naming)),
            },
            _ => PathBuf::from(self.file_name(it, Naming::Clean)),
        }
    }

    /// The roots where `it` can be, the one used for a new download first.
    fn roots(&self, it: &BaseItem, info: Option<&SeriesInfo>) -> Vec<PathBuf> {
        let mut roots = match it.item_type.as_str() {
            "Movie" => vec![self.movies_dir.clone(), self.default_dir.clone()],
            "Episode" => {
                let folder = self.series_folder(it, self.naming, info);
                let (ongoing, ended) = (&self.series_ongoing_dir, &self.series_ended_dir);
                // A series that has a folder stays there, whatever its status says.
                let first = if ongoing.join(&folder).is_dir() {
                    ongoing
                } else if ended.join(&folder).is_dir() || info.and_then(|i| i.ended) == Some(true) {
                    ended
                } else {
                    ongoing
                };
                let others = [ongoing, ended, &self.default_dir];
                std::iter::once(first.clone())
                    .chain(others.into_iter().filter(|r| *r != first).cloned())
                    .collect()
            }
            _ => vec![self.default_dir.clone()],
        };
        roots.dedup();
        roots
    }

    /// Where `it` is saved.
    pub fn target(&self, it: &BaseItem, info: Option<&SeriesInfo>) -> PathBuf {
        let root = self.roots(it, info).swap_remove(0);
        root.join(self.relative(it, self.naming, info))
    }

    /// Every place where a complete copy of `it` can be: the target first, then
    /// the other folders, the other naming, and the layouts of older versions.
    pub fn candidates(&self, it: &BaseItem, info: Option<&SeriesInfo>) -> Vec<PathBuf> {
        let other = match self.naming {
            Naming::Clean => Naming::Server,
            Naming::Server => Naming::Clean,
        };
        let mut out = vec![self.target(it, info)];
        for root in self.roots(it, info) {
            for naming in [self.naming, other] {
                out.push(root.join(self.relative(it, naming, info)));
            }
            out.extend(older_targets_for(it, &root));
        }
        let mut seen = std::collections::HashSet::new();
        out.retain(|p| seen.insert(p.clone()));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::models::MediaSource;

    fn movie() -> BaseItem {
        BaseItem {
            id: "1".into(),
            name: "Night Harbor: Return".into(),
            item_type: "Movie".into(),
            production_year: Some(2024),
            media_sources: Some(vec![MediaSource {
                size: Some(10),
                container: Some("mkv".into()),
                path: Some("/media/videos/Night Harbor Return.MP4".into()),
                ..Default::default()
            }]),
            ..Default::default()
        }
    }

    #[test]
    fn clean_names() {
        assert_eq!(clean_name("Élodie (2001)"), "Elodie.2001");
        assert_eq!(clean_name("Anna's Garden"), "Annas.Garden");
        assert_eq!(clean_name("Well-Being: A New Day"), "Well-Being.A.New.Day");
        assert_eq!(clean_name("Dr. Lemon"), "Dr.Lemon");
        assert_eq!(clean_name("Salt & Pepper!"), "Salt.Pepper");
        assert_eq!(clean_name("Straße Œuvre ÆON"), "Strasse.OEuvre.AEON");
        assert_eq!(clean_name("Łódź Đặng Ștefan ğ ı"), "Lodz.Dang.Stefan.g.i");
        assert_eq!(clean_name("2 × 3 ÷ 4"), "2.3.4", "× and ÷ are not letters");
        assert_eq!(clean_name("東京旅行 (1953)"), "東京旅行.1953");
        assert_eq!(clean_name("a/b\\c:d*e?f\"g<h>i|j"), "a.b.c.d.e.f.g.h.i.j");
        assert_eq!(clean_name("  ...trailing dots... "), "trailing.dots");
        assert_eq!(clean_name("--dash--"), "dash");
        assert_eq!(clean_name("tab\tnew\nline"), "tab.new.line");
        assert_eq!(clean_name("???"), "_");
        let long = "é".repeat(300);
        let s = clean_name(&long);
        assert!(s.len() <= MAX_NAME_BYTES);
        assert!(s.chars().all(|c| c == 'e'));
        for c in clean_name("Ça, c'est «Noël» — 100% [HD] #1 @home ~ é").chars() {
            assert!(c.is_ascii_alphanumeric() || c == '.' || c == '-', "{c:?}");
        }
    }

    #[test]
    fn legacy_names() {
        let sanitize = legacy_sanitize;
        assert_eq!(sanitize("a/b\\c:d*e?f\"g<h>i|j"), "a b c d e f g h i j");
        assert_eq!(sanitize("  trailing dots... "), "trailing dots");
        assert_eq!(sanitize("tab\tnew\nline"), "tab new line");
        assert_eq!(sanitize("???"), "_");
        let long = "é".repeat(300);
        let s = sanitize(&long);
        assert!(s.len() <= MAX_NAME_BYTES);
        assert!(s.chars().all(|c| c == 'é'));
    }

    #[test]
    fn movie_path() {
        let p = target_for(&movie(), Path::new("/dl"));
        assert_eq!(
            p,
            PathBuf::from("/dl/Night.Harbor.Return.2024/Night.Harbor.Return.2024.mp4")
        );
        assert_eq!(
            older_targets_for(&movie(), Path::new("/dl")),
            [
                PathBuf::from("/dl/Movies/Night.Harbor.Return.2024/Night.Harbor.Return.2024.mp4"),
                PathBuf::from(
                    "/dl/Movies/Night Harbor Return (2024)/Night Harbor Return (2024).mp4"
                ),
            ]
        );
    }

    #[test]
    fn year_not_doubled() {
        let mut m = movie();
        m.name = "Big Test (2024)".into();
        let p = target_for(&m, Path::new("/dl"));
        assert_eq!(p, PathBuf::from("/dl/Big.Test.2024/Big.Test.2024.mp4"));
    }

    #[test]
    fn episode_path() {
        let ep = BaseItem {
            name: "Pilot".into(),
            item_type: "Episode".into(),
            series_name: Some("The Show".into()),
            parent_index_number: Some(1),
            index_number: Some(3),
            container: Some("mov,mp4,m4a".into()),
            ..Default::default()
        };
        let p = target_for(&ep, Path::new("/dl"));
        assert_eq!(
            p,
            PathBuf::from("/dl/The.Show/S01/The.Show.S01E03.Pilot.mov")
        );
        assert_eq!(
            older_targets_for(&ep, Path::new("/dl")),
            [
                PathBuf::from("/dl/Shows/The.Show/S01/The.Show.S01E03.Pilot.mov"),
                PathBuf::from("/dl/Shows/The Show/Season 01/The Show - S01E03 - Pilot.mov"),
            ]
        );
        let no_series = BaseItem {
            series_name: None,
            ..ep
        };
        assert_eq!(
            target_for(&no_series, Path::new("/dl")),
            PathBuf::from("/dl/Unknown/S01/Unknown.S01E03.Pilot.mov")
        );
    }

    #[test]
    fn other_videos_at_the_top() {
        let it = BaseItem {
            item_type: "Video".into(),
            name: "Holiday 2025".into(),
            ..Default::default()
        };
        assert_eq!(
            target_for(&it, Path::new("/dl")),
            PathBuf::from("/dl/Holiday.2025.mkv")
        );
        assert_eq!(
            older_targets_for(&it, Path::new("/dl"))[0],
            PathBuf::from("/dl/Other/Holiday.2025.mkv")
        );
    }

    #[test]
    fn extension_fallback() {
        let it = BaseItem {
            item_type: "Video".into(),
            name: "x".into(),
            ..Default::default()
        };
        assert_eq!(extension(&it), "mkv");
    }

    fn server_layout(root: &Path) -> DownloadLayout {
        DownloadLayout {
            default_dir: root.join("other"),
            movies_dir: root.join("films"),
            series_ongoing_dir: root.join("en-cours"),
            series_ended_dir: root.join("finies"),
            naming: Naming::Server,
        }
    }

    fn file(path: &str) -> Vec<MediaSource> {
        vec![MediaSource {
            path: Some(path.into()),
            ..Default::default()
        }]
    }

    #[test]
    fn server_names_for_movies() {
        let m = BaseItem {
            name: "Wicked : Partie II".into(),
            item_type: "Movie".into(),
            production_year: Some(2025),
            media_sources: Some(file("/data/films/Wicked Partie II 2025.mkv")),
            ..Default::default()
        };
        let l = server_layout(Path::new("/dl"));
        assert_eq!(
            l.target(&m, None),
            PathBuf::from("/dl/films/Wicked Partie II 2025.mkv")
        );
        // Windows paths on the server work too.
        let w = BaseItem {
            media_sources: Some(file(r"D:\films\Wicked Partie II 2025.mkv")),
            ..m.clone()
        };
        assert_eq!(l.target(&w, None), l.target(&m, None));
        // No path: name and year.
        let n = BaseItem {
            media_sources: None,
            container: Some("mkv".into()),
            ..m
        };
        assert_eq!(
            l.target(&n, None),
            PathBuf::from("/dl/films/Wicked Partie II (2025).mkv")
        );
    }

    fn episode() -> BaseItem {
        BaseItem {
            name: "Pilot".into(),
            item_type: "Episode".into(),
            series_name: Some("Ted Lasso".into()),
            series_id: Some("s".into()),
            parent_index_number: Some(1),
            index_number: Some(1),
            media_sources: Some(file(
                "/tv/Ted Lasso/Ted.Lasso.S01E01.MULTi.VFi.WiTH.AD.1080p.WEBRip.AC3.5.1.H265-TARDiS.mkv",
            )),
            ..Default::default()
        }
    }

    #[test]
    fn series_go_to_the_folder_of_their_status() {
        let dir = tempfile::tempdir().unwrap();
        let l = server_layout(dir.path());
        let rel = "Ted Lasso (2020)/S01/Ted.Lasso.S01E01.MULTi.VFi.WiTH.AD.1080p.WEBRip.AC3.5.1.H265-TARDiS.mkv";
        let info = |ended| SeriesInfo {
            year: Some(2020),
            ended,
        };
        let ep = episode();
        assert_eq!(
            l.target(&ep, Some(&info(Some(false)))),
            l.series_ongoing_dir.join(rel)
        );
        assert_eq!(
            l.target(&ep, Some(&info(None))),
            l.series_ongoing_dir.join(rel)
        );
        assert_eq!(
            l.target(&ep, None),
            l.series_ongoing_dir.join(rel.replace(" (2020)", ""))
        );
        assert_eq!(
            l.target(&ep, Some(&info(Some(true)))),
            l.series_ended_dir.join(rel)
        );
        // A series that already has a folder stays there, whatever its status.
        std::fs::create_dir_all(l.series_ongoing_dir.join("Ted Lasso (2020)")).unwrap();
        assert_eq!(
            l.target(&ep, Some(&info(Some(true)))),
            l.series_ongoing_dir.join(rel)
        );
    }

    #[test]
    fn finds_a_file_in_another_series_folder() {
        let dir = tempfile::tempdir().unwrap();
        let l = server_layout(dir.path());
        let info = SeriesInfo {
            year: Some(2020),
            ended: Some(false),
        };
        let ep = episode();
        let there = l
            .series_ended_dir
            .join("Ted Lasso (2020)/S01/Ted.Lasso.S01E01.MULTi.VFi.WiTH.AD.1080p.WEBRip.AC3.5.1.H265-TARDiS.mkv");
        assert!(l.candidates(&ep, Some(&info)).contains(&there));
    }

    #[test]
    fn year_not_doubled_in_series_folder() {
        let l = server_layout(Path::new("/dl"));
        let mut ep = episode();
        ep.series_name = Some("Ted Lasso (2020)".into());
        let info = SeriesInfo {
            year: Some(2020),
            ended: None,
        };
        assert!(
            l.target(&ep, Some(&info))
                .starts_with("/dl/en-cours/Ted Lasso (2020)/S01")
        );
    }

    #[test]
    fn clean_naming_uses_the_type_folders() {
        let mut l = server_layout(Path::new("/dl"));
        l.naming = Naming::Clean;
        assert_eq!(
            l.target(&movie(), None),
            PathBuf::from("/dl/films/Night.Harbor.Return.2024/Night.Harbor.Return.2024.mp4")
        );
    }
}
