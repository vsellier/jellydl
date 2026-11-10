//! Find how the terminal can show images.
//!
//! The ratatui-image query works in most terminals. Inside tmux it talks to the outer
//! terminal through passthrough, so tmux does not know about the images: they stay on
//! screen or disappear when you change pane or window. When tmux can draw sixel images
//! itself (tmux 3.4+ built with sixel, outer terminal with sixel), we use that instead.

use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use ratatui_image::FontSize;
use ratatui_image::picker::{Picker, ProtocolType};

/// What tmux says about images.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct TmuxInfo {
    /// tmux was built with sixel support (its device attributes contain 4).
    pub sixel_built: bool,
    /// The outer terminal supports sixel (`sixel` in `client_termfeatures`).
    pub outer_sixel: bool,
    /// Cell size in pixels, when tmux knows it.
    pub cell: Option<(u16, u16)>,
}

/// Picker for this terminal, or `None` when the query fails.
/// Call it after the terminal is in raw mode and before the key reader starts.
pub fn detect() -> Option<Picker> {
    // The ratatui-image query reads stdin in a thread that stays blocked when the
    // terminal never answers: it then eats every key. Ask first with crossterm,
    // which gives up after 2 s and keeps the keys typed meanwhile.
    if let Err(e) = crossterm::terminal::supports_keyboard_enhancement() {
        tracing::warn!("the terminal does not answer queries ({e}); no posters");
        return None;
    }
    let base = match Picker::from_query_stdio() {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!("cannot query the terminal for images ({e}); no posters");
            return None;
        }
    };
    let tmux = std::env::var_os("TMUX")
        .is_some_and(|v| !v.is_empty())
        .then(tmux_info);
    if let Some(t) = &tmux {
        tracing::info!("posters: inside tmux, {t:?}");
    }
    let p = choose(base, tmux.as_ref());
    tracing::info!(
        "posters: {:?} protocol, font {:?}",
        p.protocol_type(),
        p.font_size()
    );
    Some(p)
}

/// Use tmux's own sixel support when it has it. Else keep the crate's choice.
pub fn choose(base: Picker, tmux: Option<&TmuxInfo>) -> Picker {
    match tmux {
        Some(t) if t.sixel_built && t.outer_sixel => {
            // `from_fontsize` does not wrap output in tmux passthrough: tmux gets the sixel data.
            // It is deprecated, but it is the only way to give our own cell size.
            #[allow(deprecated)]
            let mut p =
                Picker::from_fontsize(t.cell.map(FontSize::from).unwrap_or(base.font_size()));
            p.set_protocol_type(ProtocolType::Sixel);
            p
        }
        _ => base,
    }
}

fn tmux_info() -> TmuxInfo {
    let mut info = TmuxInfo {
        sixel_built: da1_has_sixel(),
        ..Default::default()
    };
    let out = Command::new("tmux")
        .args([
            "display",
            "-p",
            "#{client_termfeatures}|#{client_cell_width}|#{client_cell_height}",
        ])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output();
    if let Ok(out) = out
        && out.status.success()
    {
        let (outer_sixel, cell) = parse_display(&String::from_utf8_lossy(&out.stdout));
        info.outer_sixel = outer_sixel;
        info.cell = cell;
    }
    info
}

/// Parse "features|width|height" from `tmux display`.
fn parse_display(s: &str) -> (bool, Option<(u16, u16)>) {
    let mut parts = s.trim().split('|');
    let features = parts.next().unwrap_or("");
    let sixel = features.split(',').any(|f| f.trim() == "sixel");
    let num = |p: Option<&str>| {
        p.and_then(|v| v.trim().parse::<u16>().ok())
            .filter(|n| *n > 0)
    };
    let cell = num(parts.next()).zip(num(parts.next()));
    (sixel, cell)
}

/// Ask the terminal (here tmux itself, not the outer terminal) for its primary
/// device attributes, and check for sixel (4).
fn da1_has_sixel() -> bool {
    let mut out = std::io::stdout();
    if out.write_all(b"\x1b[c").and_then(|_| out.flush()).is_err() {
        return false;
    }
    let (tx, rx) = mpsc::channel();
    // The read blocks until tmux answers. tmux always answers this query.
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let mut byte = [0u8; 1];
        let mut stdin = std::io::stdin();
        while buf.len() < 64 && stdin.read_exact(&mut byte).is_ok() {
            buf.push(byte[0]);
            if byte[0] == b'c' {
                break;
            }
        }
        let _ = tx.send(buf);
    });
    rx.recv_timeout(Duration::from_millis(500))
        .is_ok_and(|reply| parse_da1(&reply))
}

/// True when a reply like "\x1b[?1;2;4c" lists 4 (sixel).
fn parse_da1(reply: &[u8]) -> bool {
    let s = String::from_utf8_lossy(reply);
    let Some(start) = s.find("\x1b[?") else {
        return false;
    };
    let body = &s[start + 3..];
    let Some(end) = body.find('c') else {
        return false;
    };
    body[..end].split(';').any(|p| p == "4")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_attributes() {
        assert!(parse_da1(b"\x1b[?1;2;4c"));
        assert!(!parse_da1(b"\x1b[?1;2c"));
        assert!(!parse_da1(b"\x1b[?64;14c"), "14 is not 4");
        assert!(!parse_da1(b"garbage"));
    }

    #[test]
    fn tmux_display_output() {
        assert_eq!(
            parse_display("256,RGB,sixel,title|9|18\n"),
            (true, Some((9, 18)))
        );
        assert_eq!(parse_display("256,RGB|0|0\n"), (false, None));
        assert_eq!(parse_display("|\n"), (false, None));
    }

    #[test]
    fn sixel_in_tmux_only_when_both_sides_support_it() {
        #[allow(deprecated)]
        let base = || Picker::from_fontsize((10, 20).into());
        let font = |p: &Picker| (p.font_size().width, p.font_size().height);
        let both = TmuxInfo {
            sixel_built: true,
            outer_sixel: true,
            cell: Some((9, 18)),
        };
        let p = choose(base(), Some(&both));
        assert_eq!(p.protocol_type(), ProtocolType::Sixel);
        assert_eq!(font(&p), (9, 18));

        let not_built = TmuxInfo {
            sixel_built: false,
            ..both.clone()
        };
        assert_eq!(
            choose(base(), Some(&not_built)).protocol_type(),
            base().protocol_type()
        );
        let no_cell = TmuxInfo { cell: None, ..both };
        assert_eq!(font(&choose(base(), Some(&no_cell))), (10, 20));
        assert_eq!(choose(base(), None).protocol_type(), base().protocol_type());
    }
}
