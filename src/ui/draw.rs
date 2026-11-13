//! Rendering. Reads the App state, never changes it (except list scroll state).

use std::collections::HashMap;

use ratatui::Frame;
use ratatui::layout::{Constraint, Flex, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Cell, Clear, List, ListItem, Paragraph, Row, Table, Tabs, Wrap,
};
use ratatui_image::{FilterType, Resize, StatefulImage};

use super::app::{App, Cast, Popup, Poster, PosterCache, Tab, downloaded_path};
use super::fmt;
use super::view::ListView;
use crate::api::{BaseItem, NewSet, SortField, date};
use crate::download::{JobState, Totals};
use crate::paths;
use crate::paths::{DownloadLayout, SeriesInfo};

const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
const ACCENT: Color = Color::Cyan;

pub fn draw(f: &mut Frame, app: &mut App) {
    let [top, main, status] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(1),
    ])
    .areas(f.area());
    draw_tabs(f, app, top);
    match app.tab {
        Tab::Library => draw_library(f, app, main),
        Tab::Search => draw_search(f, app, main),
        Tab::Downloads => draw_downloads(f, app, main),
    }
    draw_status(f, app, status);
    if let Some(p) = &app.popup {
        draw_popup(f, p, app.last_check);
    }
}

fn spinner(app: &App) -> &'static str {
    SPINNER[(app.ticks as usize) % SPINNER.len()]
}

fn draw_tabs(f: &mut Frame, app: &App, area: Rect) {
    let (r, q) = app.active_count();
    let dl = if r + q > 0 {
        format!(" 3 Downloads ({r}↓ {q}…) ")
    } else {
        " 3 Downloads ".to_string()
    };
    let tabs = Tabs::new(vec![
        " 1 Library ".to_string(),
        " 2 Search ".to_string(),
        dl,
    ])
    .select(match app.tab {
        Tab::Library => 0,
        Tab::Search => 1,
        Tab::Downloads => 2,
    })
    .style(Style::new().fg(Color::Gray))
    .highlight_style(Style::new().fg(Color::Black).bg(ACCENT).bold())
    .divider("")
    .padding("", " ");
    let [left, right] =
        Layout::horizontal([Constraint::Min(0), Constraint::Length(10)]).areas(area);
    f.render_widget(tabs, left);
    f.render_widget(Line::from("? help ").right_aligned().dark_gray(), right);
}

fn draw_library(f: &mut Frame, app: &mut App, area: Rect) {
    let (layout, series, spin) = (app.layout.clone(), app.series_info.clone(), spinner(app));
    let ctx = DetailCtx {
        cast: &app.cast,
        posters: &mut app.posters,
        // An image under a popup could stay on screen.
        show_poster: app.popup.is_none(),
        layout: &layout,
        series: &series,
        new: &app.new_set,
    };
    if let Some(v) = app.lib_stack.last_mut() {
        draw_list_and_detail(f, v, ctx, area, spin);
    }
}

fn draw_search(f: &mut Frame, app: &mut App, area: Rect) {
    let [input, rest] = Layout::vertical([Constraint::Length(3), Constraint::Min(0)]).areas(area);
    let style = if app.search_editing {
        Style::new().fg(ACCENT)
    } else {
        Style::new().fg(Color::DarkGray)
    };
    let hint = if app.search_editing {
        " Enter: search · ←→ Home End ^A ^E: move · Esc/↓: results "
    } else {
        " / to type "
    };
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(style)
        .title(" Search videos ")
        .title_bottom(Line::from(hint).right_aligned());
    let (visible, cursor) = app
        .search_input
        .view(input.width.saturating_sub(2) as usize);
    let text = if app.search_input.is_empty() && !app.search_editing {
        Line::from("Press / and type a title").dark_gray()
    } else {
        Line::from(visible)
    };
    f.render_widget(Paragraph::new(text).block(block), input);
    if app.search_editing {
        f.set_cursor_position((input.x + 1 + cursor as u16, input.y + 1));
    }
    let (layout, series, spin) = (app.layout.clone(), app.series_info.clone(), spinner(app));
    let ctx = DetailCtx {
        cast: &app.cast,
        posters: &mut app.posters,
        show_poster: app.popup.is_none(),
        layout: &layout,
        series: &series,
        new: &app.new_set,
    };
    draw_list_and_detail(f, &mut app.search, ctx, rest, spin);
}

fn draw_list_and_detail(f: &mut Frame, v: &mut ListView, ctx: DetailCtx, area: Rect, spin: &str) {
    let [list_area, detail_area] =
        Layout::horizontal([Constraint::Percentage(58), Constraint::Percentage(42)]).areas(area);

    let items: Vec<ListItem> = v
        .items
        .iter()
        .map(|it| {
            let mark = if v.is_marked(&it.id) { "● " } else { "  " };
            let tag_color = match it.item_type.as_str() {
                "Movie" => Color::Yellow,
                "Series" | "Season" => Color::Magenta,
                "Episode" => Color::Green,
                _ => Color::Blue,
            };
            let mut spans = vec![
                Span::styled(mark, Style::new().fg(ACCENT)),
                Span::styled(format!("{} ", it.kind_tag()), Style::new().fg(tag_color)),
                Span::raw(it.label()),
            ];
            if let Some(n) = it.child_count.filter(|_| !it.is_playable()) {
                spans.push(Span::styled(format!("  ({n})"), Style::new().dark_gray()));
            }
            // In a list of new items only, every row is new: no tag.
            if v.new_since.is_none() && ctx.new.is_new(it) {
                spans.push(Span::styled("  new", Style::new().fg(Color::Green).bold()));
            }
            ListItem::new(Line::from(spans))
        })
        .collect();

    let mut title = format!(" {} ", v.title);
    if v.total > 0 {
        title.push_str(&format!(
            "[{}/{}] page {}/{} ",
            v.offset + v.state.selected().map_or(0, |i| i + 1),
            v.total,
            v.current_page() + 1,
            v.page_count()
        ));
    }
    if v.source.is_paged() {
        title.push_str(&format!("· {} ", v.sort.label()));
    }
    if let Some(t) = v.new_since {
        title.push_str(&format!("· new since {} ", date::format_utc(t)));
    }
    if v.loading() {
        title.push_str(spin);
        title.push(' ');
    }
    let mut footer = String::from(" Enter open · d download · Space mark · [ ] page ");
    if v.source.is_paged() {
        footer.push_str("· s sort ");
    }
    if v.marked_count() > 0 {
        footer = format!(" {} marked · d download them ", v.marked_count());
    }
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .title(Line::from(title).bold())
        .title_bottom(Line::from(footer).dark_gray());

    if v.items.is_empty() {
        let msg = match (&v.error, v.loading()) {
            (Some(e), _) => Line::from(format!("Error: {e}")).red(),
            (None, true) => Line::from("Loading...").dark_gray(),
            (None, false) => Line::from("Nothing here").dark_gray(),
        };
        f.render_widget(
            Paragraph::new(msg).wrap(Wrap { trim: true }).block(block),
            list_area,
        );
    } else {
        let list = List::new(items)
            .block(block)
            .highlight_style(
                Style::new()
                    .bg(Color::DarkGray)
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol("▶");
        f.render_stateful_widget(list, list_area, &mut v.state);
    }

    draw_detail(f, v.selected(), ctx, detail_area);
}

/// What the details pane needs from the App, besides the item.
struct DetailCtx<'a> {
    cast: &'a HashMap<String, Cast>,
    posters: &'a mut PosterCache,
    show_poster: bool,
    layout: &'a DownloadLayout,
    series: &'a HashMap<String, SeriesInfo>,
    new: &'a NewSet,
}

/// The poster is shown only when the pane has at least this many rows inside.
const POSTER_MIN_ROWS: u16 = 16;
/// Text next to the poster needs at least this many columns.
const SIDE_TEXT_MIN_COLS: u16 = 24;

fn draw_detail(f: &mut Frame, it: Option<&BaseItem>, ctx: DetailCtx, area: Rect) {
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .title(" Details ");
    let inner = block.inner(area);
    f.render_widget(block, area);
    let Some(it) = it else { return };
    let (mut head, body) = detail_text(it, ctx.cast.get(&it.id), ctx.layout, ctx.series);
    let para = |lines: Vec<Line<'static>>| Paragraph::new(lines).wrap(Wrap { trim: false });

    let poster = match ctx.posters.get_mut(&it.id) {
        Some(Poster::Ready(p)) if ctx.show_poster && inner.height >= POSTER_MIN_ROWS => Some(p),
        _ => None,
    };
    let Some(poster) = poster else {
        head.extend(body);
        f.render_widget(para(head), inner);
        return;
    };

    // Poster at the top left, at most 45% of the height and half of the width.
    let resize = || Resize::Fit(Some(FilterType::Triangle));
    let max = Rect {
        height: (inner.height * 45 / 100).clamp(8, 20),
        width: inner.width / 2,
        ..inner
    };
    let size = poster.size_for(resize(), max.as_size());
    let img = Rect {
        width: size.width,
        height: size.height,
        ..inner
    };
    f.render_stateful_widget(
        StatefulImage::default().resize(resize()),
        img,
        poster.as_mut(),
    );

    let below = Rect {
        y: inner.y + img.height + 1,
        height: inner.height.saturating_sub(img.height + 1),
        ..inner
    };
    let side_w = inner.width.saturating_sub(img.width + 2);
    if side_w >= SIDE_TEXT_MIN_COLS {
        // Short facts next to the poster. What does not fit goes below it.
        let rest = head.split_off((img.height as usize).min(head.len()));
        let side = Rect {
            x: img.x + img.width + 2,
            width: side_w,
            height: img.height,
            ..inner
        };
        f.render_widget(Paragraph::new(head), side);
        let mut lines = rest;
        lines.extend(body);
        f.render_widget(para(lines), below);
    } else {
        head.extend(body);
        f.render_widget(para(head), below);
    }
}

/// Text of the details pane: short facts (`head`), then the rest (`body`).
fn detail_text(
    it: &BaseItem,
    cast: Option<&Cast>,
    layout: &DownloadLayout,
    series: &HashMap<String, SeriesInfo>,
) -> (Vec<Line<'static>>, Vec<Line<'static>>) {
    let mut lines = vec![Line::from(it.name.clone()).bold().fg(ACCENT)];
    let kv = |k: &str, v: String| {
        Line::from(vec![
            Span::styled(format!("{k:<9}"), Style::new().dark_gray()),
            Span::raw(v),
        ])
    };
    lines.push(kv("Type", it.item_type.clone()));
    if let Some(s) = &it.series_name {
        lines.push(kv("Series", s.clone()));
    }
    if let (Some(s), Some(e)) = (it.parent_index_number, it.index_number)
        && it.item_type == "Episode"
    {
        lines.push(kv("Episode", format!("S{s:02}E{e:02}")));
    }
    if let Some(y) = it.production_year {
        lines.push(kv("Year", y.to_string()));
    }
    if let Some(d) = it.release_date() {
        lines.push(kv("Released", d.to_string()));
    }
    if let Some(r) = it.community_rating {
        lines.push(kv("Rating", format!("{r:.1}")));
    }
    if let Some(m) = it.runtime_minutes() {
        lines.push(kv("Runtime", format!("{m} min")));
    }
    if let Some(d) = it.date_added() {
        lines.push(kv("Added", d.to_string()));
    }
    if let Some(n) = it.child_count {
        lines.push(kv("Items", n.to_string()));
    }
    if it.is_playable() {
        lines.push(kv(
            "Size",
            it.size()
                .map(fmt::bytes)
                .unwrap_or_else(|| "unknown".into()),
        ));
        lines.push(kv("Format", paths::extension(it)));
        let info = it.series_id.as_ref().and_then(|id| series.get(id));
        let status = if downloaded_path(it, layout, info).is_some() {
            Span::styled("downloaded ✓", Style::new().fg(Color::Green))
        } else {
            Span::raw("not downloaded")
        };
        lines.push(Line::from(vec![
            Span::styled(format!("{:<9}", "On disk"), Style::new().dark_gray()),
            status,
        ]));
    }
    let head = lines;
    let mut lines = Vec::new();
    if it.is_playable() {
        let info = it.series_id.as_ref().and_then(|id| series.get(id));
        match downloaded_path(it, layout, info) {
            Some(p) => lines.push(kv("File", p.display().to_string())),
            None => lines.push(kv("Save to", layout.target(it, info).display().to_string())),
        }
    } else {
        lines.push(Line::from("d downloads every item inside").dark_gray());
    }
    // The cast goes before the overview: a long overview would hide it.
    if it.has_people() {
        cast_lines(&mut lines, cast);
    }
    if let Some(o) = it.overview.as_deref().filter(|o| !o.is_empty()) {
        lines.push(Line::default());
        lines.push(Line::from(o.to_string()));
    }
    (head, lines)
}

/// Max number of actors shown in the details pane.
const MAX_ACTORS: usize = 10;

fn cast_lines(lines: &mut Vec<Line<'static>>, cast: Option<&Cast>) {
    let people = match cast {
        None | Some(Cast::Loading) => {
            lines.push(Line::default());
            lines.push(Line::from("Cast: loading…").dark_gray());
            return;
        }
        Some(Cast::Failed) => {
            lines.push(Line::default());
            lines.push(Line::from("Cast: not available").dark_gray());
            return;
        }
        Some(Cast::Loaded(p)) => p,
    };
    let names = |kind: &str| -> Vec<String> {
        people
            .iter()
            .filter(|p| p.kind == kind)
            .map(|p| p.name.clone())
            .collect()
    };
    let directors = names("Director");
    let writers = names("Writer");
    let actors: Vec<_> = people
        .iter()
        .filter(|p| matches!(p.kind.as_str(), "Actor" | "GuestStar"))
        .collect();
    if directors.is_empty() && writers.is_empty() && actors.is_empty() {
        return;
    }
    lines.push(Line::default());
    for (label, list) in [("Director", directors), ("Writer", writers)] {
        if !list.is_empty() {
            lines.push(Line::from(vec![
                Span::styled(format!("{label:<9}"), Style::new().dark_gray()),
                Span::raw(list.join(", ")),
            ]));
        }
    }
    if actors.is_empty() {
        return;
    }
    lines.push(Line::from("Cast").bold().fg(ACCENT));
    for p in actors.iter().take(MAX_ACTORS) {
        let mut spans = vec![Span::raw(format!("  {}", p.name))];
        if let Some(r) = p.role.as_deref().filter(|r| !r.is_empty()) {
            spans.push(Span::styled(format!("  as {r}"), Style::new().dark_gray()));
        }
        if p.kind == "GuestStar" {
            spans.push(Span::styled("  (guest)", Style::new().dark_gray()));
        }
        lines.push(Line::from(spans));
    }
    if actors.len() > MAX_ACTORS {
        lines.push(Line::from(format!("  … and {} more", actors.len() - MAX_ACTORS)).dark_gray());
    }
}

fn draw_downloads(f: &mut Frame, app: &mut App, area: Rect) {
    let [table_area, info_area] =
        Layout::vertical([Constraint::Min(3), Constraint::Length(5)]).areas(area);
    let header = Row::new(["Title", "State", "Progress", "Size", "Speed", "ETA"])
        .style(Style::new().fg(ACCENT).bold());
    let rows: Vec<Row> = app
        .jobs
        .iter()
        .map(|j| {
            let color = match j.state {
                JobState::Running => Color::Cyan,
                JobState::Done => Color::Green,
                JobState::Failed => Color::Red,
                JobState::Paused => Color::Yellow,
                JobState::Queued | JobState::Canceled => Color::DarkGray,
            };
            let mut state = j.state.label().to_string();
            if j.state == JobState::Running && j.retry > 0 {
                state = format!("retry {}", j.retry);
            }
            let size = match j.total {
                Some(t) if j.state != JobState::Done => {
                    format!("{} / {}", fmt::bytes(j.downloaded), fmt::bytes(t))
                }
                Some(t) => fmt::bytes(t),
                None => fmt::bytes(j.downloaded),
            };
            Row::new(vec![
                Cell::from(j.title.clone()),
                Cell::from(state).style(Style::new().fg(color)),
                Cell::from(fmt::bar(j.fraction(), 14)).style(Style::new().fg(color)),
                Cell::from(size),
                Cell::from(fmt::speed(j.speed)),
                Cell::from(j.eta_secs().map(fmt::duration).unwrap_or_default()),
            ])
        })
        .collect();
    let widths = [
        Constraint::Fill(1),
        Constraint::Length(9),
        Constraint::Length(19),
        Constraint::Length(21),
        Constraint::Length(11),
        Constraint::Length(7),
    ];
    let (r, q) = app.active_count();
    let limit = app
        .speed_limit
        .map(|b| format!(" · limit {}/s", fmt::bytes(b)))
        .unwrap_or_default();
    let title = format!(
        " Downloads · {r}/{} running · {q} queued · {}{limit} ",
        app.max_parallel,
        if app.total_speed() > 0.0 {
            fmt::speed(app.total_speed())
        } else {
            "idle".into()
        }
    );
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .title(Line::from(title).bold())
        .title(
            Line::from(totals_text(app, true))
                .right_aligned()
                .fg(ACCENT),
        )
        .title_bottom(
            Line::from(
                " p pause/resume · x cancel · r retry · P/R all · c clear done · +/- parallel ",
            )
            .dark_gray(),
        );
    if app.jobs.is_empty() {
        let p =
            Paragraph::new("No downloads yet. Select an item in Library or Search and press d.")
                .dark_gray()
                .block(block);
        f.render_widget(p, table_area);
    } else {
        let table = Table::new(rows, widths)
            .header(header)
            .block(block)
            .row_highlight_style(Style::new().bg(Color::DarkGray).bold())
            .highlight_symbol("▶");
        f.render_stateful_widget(table, table_area, &mut app.dl_state);
    }

    let sel = app.dl_state.selected().and_then(|i| app.jobs.get(i));
    let mut lines = Vec::new();
    if let Some(j) = sel {
        lines.push(Line::from(vec![
            Span::raw("File   ").dark_gray(),
            Span::raw(j.target.display().to_string()),
        ]));
        if let Some(e) = &j.error {
            lines.push(Line::from(vec![
                Span::raw("Error  ").dark_gray(),
                Span::raw(e.clone()).red(),
            ]));
        }
    }
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .title(" Selected ");
    f.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: true }).block(block),
        info_area,
    );
}

fn draw_status(f: &mut Frame, app: &App, area: Rect) {
    let mut left = vec![
        Span::styled(
            format!(" {} ", app.api.user_name),
            Style::new().fg(Color::Black).bg(ACCENT),
        ),
        Span::raw(format!(" {} ", app.api.base_url())).dark_gray(),
    ];
    if let Some(p) = &app.proxy_label {
        left.push(Span::styled(
            format!(" via {p} "),
            Style::new().fg(Color::Yellow),
        ));
    }
    if let Some(m) = &app.message {
        let style = if m.error {
            Style::new().fg(Color::Red)
        } else {
            Style::new().fg(Color::Green)
        };
        left.push(Span::styled(format!(" {}", m.text), style));
    }
    let (r, q) = app.active_count();
    let right = format!(
        "{}↓ {r}/{} {}  queued {q} ",
        totals_text(app, false),
        app.max_parallel,
        fmt::speed(app.total_speed())
    );
    let [l, rt] = Layout::horizontal([
        Constraint::Min(0),
        Constraint::Length(right.chars().count() as u16),
    ])
    .areas(area);
    f.render_widget(Line::from(left), l);
    f.render_widget(Line::from(right).dark_gray(), rt);
}

/// "1.2 GB / 4.0 GB · 30% · ETA 12m05s" for the unfinished downloads.
/// Empty when there are none. `long` adds the percent and the unknown sizes.
fn totals_text(app: &App, long: bool) -> String {
    let t = Totals::of(&app.jobs);
    if t.count == 0 {
        return String::new();
    }
    let mut parts = vec![format!(
        "{} / {}",
        fmt::bytes(t.downloaded),
        fmt::bytes(t.total)
    )];
    if long && let Some(f) = t.fraction() {
        parts.push(format!("{}%", (f * 100.0) as u32));
    }
    match t.eta_secs(app.total_speed()) {
        Some(s) => parts.push(format!("ETA {}", fmt::duration(s))),
        None if long && t.remaining > 0 => parts.push("ETA ?".into()),
        None => {}
    }
    if long && t.unknown > 0 {
        parts.push(format!("+{} of unknown size", t.unknown));
    }
    format!(" {} ", parts.join(" · "))
}

fn centered(area: Rect, w: u16, h: u16) -> Rect {
    let [v] = Layout::vertical([Constraint::Length(h)])
        .flex(Flex::Center)
        .areas(area);
    let [r] = Layout::horizontal([Constraint::Length(w)])
        .flex(Flex::Center)
        .areas(v);
    r
}

fn draw_popup(f: &mut Frame, p: &Popup, last_check: i64) {
    let (title, lines, w): (&str, Vec<Line>, u16) = match p {
        Popup::Help => (" Help ", help_lines(), 64),
        Popup::Sort { cursor, descending } => (" Sort by ", sort_lines(*cursor, *descending), 40),
        Popup::ConfirmQuit => (
            " Quit? ",
            vec![
                Line::from("Downloads are still running or queued."),
                Line::from("They will resume the next time you start jellydl."),
                Line::default(),
                Line::from("y / Enter: quit     any other key: stay").bold(),
            ],
            56,
        ),
        Popup::ConfirmMarkSeen => (
            " Mark all as seen? ",
            vec![
                Line::from(format!("Last check: {}.", date::format_utc(last_check))),
                Line::from("Set it to now: the new items are no longer new."),
                Line::default(),
                Line::from("y / Enter: set it to now     any other key: cancel").bold(),
            ],
            60,
        ),
        Popup::ConfirmEnqueue {
            jobs,
            size,
            unknown_size,
            skipped,
        } => {
            let mut l = vec![Line::from(format!(
                "Download {} file(s), {} in total?",
                jobs.len(),
                fmt::bytes(*size)
            ))];
            if *unknown_size > 0 {
                l.push(
                    Line::from(format!("{unknown_size} file(s) have an unknown size.")).dark_gray(),
                );
            }
            if *skipped > 0 {
                l.push(
                    Line::from(format!(
                        "{skipped} skipped: already on disk or in the queue."
                    ))
                    .dark_gray(),
                );
            }
            for j in jobs.iter().take(6) {
                l.push(Line::from(format!("  • {}", j.title)).dark_gray());
            }
            if jobs.len() > 6 {
                l.push(Line::from(format!("  … and {} more", jobs.len() - 6)).dark_gray());
            }
            l.push(Line::default());
            l.push(Line::from("y / Enter: download     any other key: cancel").bold());
            (" Confirm download ", l, 64)
        }
    };
    let area = f.area();
    let rect = centered(
        area,
        w.min(area.width),
        (lines.len() as u16 + 2).min(area.height),
    );
    f.render_widget(Clear, rect);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(ACCENT))
        .title(title);
    f.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .block(block),
        rect,
    );
}

fn sort_lines(cursor: usize, descending: bool) -> Vec<Line<'static>> {
    let mut l: Vec<Line> = SortField::ALL
        .iter()
        .enumerate()
        .map(|(i, f)| {
            if i == cursor {
                Line::from(format!("▶ {}", f.label())).bold().fg(ACCENT)
            } else {
                Line::from(format!("  {}", f.label()))
            }
        })
        .collect();
    l.push(Line::default());
    let (on, off) = (
        Style::new().fg(Color::Black).bg(ACCENT).bold(),
        Style::new().dark_gray(),
    );
    let (asc, desc) = if descending { (off, on) } else { (on, off) };
    l.push(Line::from(vec![
        Span::raw("Order  "),
        Span::styled(" ↑ Ascending ", asc),
        Span::raw(" "),
        Span::styled(" ↓ Descending ", desc),
    ]));
    l.push(Line::default());
    l.push(Line::from("↑↓ field · ←→ Space order").dark_gray());
    l.push(Line::from("Enter apply · Esc cancel").dark_gray());
    l
}

fn help_lines() -> Vec<Line<'static>> {
    let rows: &[(&str, &str)] = &[
        ("Tab / 1 2 3", "switch tab"),
        ("↑↓ j k  PgUp PgDn  g G", "move (also in Downloads)"),
        ("← → Home End ^A ^E", "move the cursor in the search box"),
        ("Enter → l", "open the selected item"),
        ("Esc ← h Backspace", "go back"),
        ("/", "search"),
        ("d", "download selection (folders: all inside)"),
        ("Space / a", "mark one / mark all"),
        ("r", "reload the list"),
        ("[ ] / { }", "previous / next page, first / last page"),
        ("s", "sort menu (library and folder lists)"),
        ("o", "reverse the sort order"),
        ("n", "show only new items / show all"),
        ("N", "mark all as seen (last check = now)"),
        ("", ""),
        ("Downloads tab", ""),
        ("p Space", "pause / resume"),
        ("x Del", "cancel; on a stopped download: remove it"),
        ("r", "retry a failed or canceled download"),
        ("P / R", "pause all / resume all"),
        ("c", "clear done and canceled"),
        ("+ / -", "more / fewer parallel downloads"),
        ("", ""),
        ("q", "quit (the queue is saved)"),
    ];
    rows.iter()
        .map(|(k, d)| {
            if d.is_empty() {
                Line::from(k.to_string()).bold().fg(ACCENT)
            } else {
                Line::from(vec![
                    Span::styled(format!("{k:<24}"), Style::new().fg(Color::Yellow)),
                    Span::raw(d.to_string()),
                ])
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui_image::picker::Picker;

    fn movie() -> BaseItem {
        BaseItem {
            id: "m1".into(),
            name: "Some Film".into(),
            item_type: "Movie".into(),
            production_year: Some(1999),
            ..Default::default()
        }
    }

    /// Draw a list of two movies, one added after the last check.
    fn render_list(new_since: Option<i64>) -> Vec<String> {
        use super::super::view::Source;
        let mut term = Terminal::new(TestBackend::new(100, 10)).unwrap();
        let mut v = ListView::new(1, "Movies", Source::Libraries);
        let mut old = movie();
        old.name = "Old Film".into();
        old.date_created = Some("2024-01-01T00:00:00Z".into());
        let mut new = movie();
        new.id = "m2".into();
        new.name = "New Film".into();
        new.date_created = Some("2024-06-01T00:00:00Z".into());
        v.items = vec![old, new];
        v.new_since = new_since;
        let (cast, mut posters) = (HashMap::new(), PosterCache::default());
        let set = NewSet::empty(date::parse_iso("2024-03-01T00:00:00Z").unwrap());
        term.draw(|f| {
            let ctx = DetailCtx {
                cast: &cast,
                posters: &mut posters,
                show_poster: false,
                layout: &DownloadLayout::single(std::path::PathBuf::from("/tmp/x")),
                series: &HashMap::new(),
                new: &set,
            };
            draw_list_and_detail(f, &mut v, ctx, f.area(), "");
        })
        .unwrap();
        let buf = term.backend().buffer().clone();
        (0..buf.area.height)
            .map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect())
            .collect()
    }

    #[test]
    fn new_items_get_a_tag() {
        let lines = render_list(None);
        let row = |name: &str| lines.iter().find(|l| l.contains(name)).unwrap().clone();
        assert!(row("New Film").contains("new"), "{lines:#?}");
        assert!(!row("Old Film").contains("new"), "{lines:#?}");

        // A list of new items only: no tag, the title says since when.
        let lines = render_list(Some(0));
        assert!(
            lines[0].contains("new since 1970-01-01 00:00 UTC"),
            "{lines:#?}"
        );
        let row = lines.iter().find(|l| l.contains("New Film")).unwrap();
        assert!(!row.contains("  new"), "{row}");
    }

    /// Draw the details pane and return the screen as text lines.
    fn render(posters: &mut PosterCache, show_poster: bool) -> Vec<String> {
        let mut term = Terminal::new(TestBackend::new(70, 40)).unwrap();
        let cast = HashMap::new();
        let it = movie();
        term.draw(|f| {
            let ctx = DetailCtx {
                cast: &cast,
                posters,
                show_poster,
                layout: &DownloadLayout::single(std::path::PathBuf::from("/tmp/x")),
                series: &HashMap::new(),
                new: &NewSet::empty(0),
            };
            draw_detail(f, Some(&it), ctx, f.area());
        })
        .unwrap();
        let buf = term.backend().buffer().clone();
        (0..buf.area.height)
            .map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect())
            .collect()
    }

    fn col_of(lines: &[String], text: &str) -> Option<usize> {
        lines
            .iter()
            .find_map(|l| l.find(text).map(|i| l[..i].chars().count()))
    }

    #[test]
    fn poster_is_drawn_left_of_the_facts() {
        // A portrait image, drawn with half blocks as in a terminal without image support.
        let img = image::DynamicImage::new_rgb8(200, 300);
        let mut posters = PosterCache::default();
        let p = Picker::halfblocks().new_resize_protocol(img);
        posters.insert("m1".into(), Poster::Ready(Box::new(p)));

        let with = render(&mut posters, true);
        let col = col_of(&with, "Some Film").expect("name is shown");
        assert!(col > 5, "the name is right of the poster (column {col})");
        assert!(col_of(&with, "Save to").is_some(), "the rest is below");

        // Under a popup, no poster: the text starts at the left border.
        let without = render(&mut posters, false);
        assert_eq!(col_of(&without, "Some Film"), Some(1));
    }

    #[test]
    fn no_poster_while_loading() {
        let mut posters = PosterCache::default();
        posters.insert("m1".into(), Poster::Loading);
        assert_eq!(col_of(&render(&mut posters, true), "Some Film"), Some(1));
    }
}
