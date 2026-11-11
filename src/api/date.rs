//! Dates as Unix seconds (UTC), without a date library.
//!
//! Jellyfin sends dates such as "2024-03-01T12:00:00.0000000Z".

use std::time::{SystemTime, UNIX_EPOCH};

/// Seconds since 1970-01-01 UTC, now.
pub fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

/// Days from 1970-01-01 to the date (proleptic Gregorian calendar).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// (year, month, day) of a day number from 1970-01-01.
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

/// Parse "YYYY-MM-DDTHH:MM:SS[.fraction][Z|±HH:MM]". No zone means UTC.
pub fn parse_iso(s: &str) -> Option<i64> {
    let s = s.trim();
    let num = |a: usize, b: usize| s.get(a..b)?.parse::<i64>().ok();
    let sep = |i: usize, c: u8| s.as_bytes().get(i) == Some(&c);
    if !(sep(4, b'-') && sep(7, b'-') && (sep(10, b'T') || sep(10, b' '))) {
        return None;
    }
    let (y, mo, d) = (num(0, 4)?, num(5, 7)?, num(8, 10)?);
    let (h, mi) = (num(11, 13)?, num(14, 16)?);
    let sec = if sep(16, b':') { num(17, 19)? } else { 0 };
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || sec > 60 {
        return None;
    }
    // Skip the fraction, then read the zone.
    let rest = s.get(19.min(s.len())..).unwrap_or("");
    let rest = rest.trim_start_matches(|c: char| c == '.' || c.is_ascii_digit());
    let offset = match rest.as_bytes().first() {
        None | Some(b'Z' | b'z') => 0,
        Some(&c @ (b'+' | b'-')) => {
            let z = &rest[1..];
            let oh: i64 = z.get(0..2)?.parse().ok()?;
            let om: i64 = z
                .get(2..)
                .map(|r| r.trim_start_matches(':'))
                .map_or(Some(0), |r| {
                    if r.is_empty() {
                        Some(0)
                    } else {
                        r.parse().ok()
                    }
                })?;
            let o = oh * 3600 + om * 60;
            if c == b'+' { o } else { -o }
        }
        Some(_) => return None,
    };
    Some(days_from_civil(y, mo, d) * 86_400 + h * 3600 + mi * 60 + sec - offset)
}

/// "2026-11-11 14:05 UTC".
pub fn format_utc(t: i64) -> String {
    let (y, m, d) = civil_from_days(t.div_euclid(86_400));
    let s = t.rem_euclid(86_400);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02} UTC",
        s / 3600,
        s / 60 % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_jellyfin_dates() {
        assert_eq!(parse_iso("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(
            parse_iso("2024-03-01T12:00:00.0000000Z"),
            Some(1_709_294_400)
        );
        assert_eq!(parse_iso("2024-03-01T12:00:00"), Some(1_709_294_400));
        assert_eq!(
            parse_iso("2024-03-01T14:00:00.123+02:00"),
            Some(1_709_294_400)
        );
        assert_eq!(parse_iso("2024-03-01T11:30:00-0030"), Some(1_709_294_400));
        assert_eq!(parse_iso("2000-02-29T00:00:00Z"), Some(951_782_400));
        assert_eq!(parse_iso("garbage"), None);
        assert_eq!(parse_iso("2024-13-01T00:00:00Z"), None);
    }

    #[test]
    fn formats_and_round_trips() {
        assert_eq!(format_utc(1_709_294_400), "2024-03-01 12:00 UTC");
        assert_eq!(format_utc(0), "1970-01-01 00:00 UTC");
        for t in [0, 951_782_400, 1_709_294_400, 4_102_444_799] {
            let (y, m, d) = civil_from_days(t / 86_400);
            assert_eq!(days_from_civil(y, m, d), t / 86_400);
        }
    }
}
