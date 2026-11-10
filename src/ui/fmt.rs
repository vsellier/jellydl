//! Number formatting for the UI.

pub fn bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", UNITS[u])
    }
}

pub fn speed(bps: f64) -> String {
    if bps < 1.0 {
        String::new()
    } else {
        format!("{}/s", bytes(bps as u64))
    }
}

pub fn duration(secs: u64) -> String {
    let (h, m, s) = (secs / 3600, secs / 60 % 60, secs % 60);
    if h > 0 {
        format!("{h}h{m:02}m")
    } else if m > 0 {
        format!("{m}m{s:02}s")
    } else {
        format!("{s}s")
    }
}

/// Text progress bar, e.g. `██████░░░░  60%`.
pub fn bar(fraction: Option<f64>, width: usize) -> String {
    match fraction {
        Some(f) => {
            let full = (f * width as f64).round() as usize;
            format!(
                "{}{} {:>3}%",
                "█".repeat(full),
                "░".repeat(width - full),
                (f * 100.0) as u32
            )
        }
        None => format!("{} ?", "░".repeat(width)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats() {
        assert_eq!(bytes(512), "512 B");
        assert_eq!(bytes(1536), "1.5 KB");
        assert_eq!(bytes(3 * 1024 * 1024 * 1024), "3.0 GB");
        assert_eq!(duration(59), "59s");
        assert_eq!(duration(61), "1m01s");
        assert_eq!(duration(3700), "1h01m");
        assert_eq!(bar(Some(0.5), 4), "██░░  50%");
    }
}
