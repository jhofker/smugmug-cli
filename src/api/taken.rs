//! Capture times as SmugMug stores them.
//!
//! SmugMug reads EXIF `DateTimeOriginal` (the camera's clock, no time zone)
//! as US Pacific time, observing daylight saving, and stores the resulting
//! UTC instant. It ignores `OffsetTimeOriginal` and the account's own time
//! zone. Verified 2026-10-02 on an `America/Chicago` account: 12:00 on
//! 2026-05-01 was stored as 19:00Z, and 12:00 on 2026-01-15 as 20:00Z.
//!
//! So a camera clock time maps to a UTC instant through Pacific time, and
//! back the same way.

use anyhow::Result;
use chrono::{DateTime, Duration, LocalResult, NaiveDate, NaiveDateTime, TimeZone, Utc};
use chrono_tz::America::Los_Angeles;

/// The earliest and latest UTC instants SmugMug may store for a camera clock
/// time: the same instant twice, or the two candidates when daylight saving
/// makes the clock time ambiguous (the repeated hour in November) or skipped
/// (in March).
pub fn stored_span(camera_time: NaiveDateTime) -> (DateTime<Utc>, DateTime<Utc>) {
    match Los_Angeles.from_local_datetime(&camera_time) {
        LocalResult::Single(t) => (t.to_utc(), t.to_utc()),
        LocalResult::Ambiguous(a, b) => {
            let (a, b) = (a.to_utc(), b.to_utc());
            (a.min(b), a.max(b))
        }
        // In the hour skipped by springing forward: PDT or PST.
        LocalResult::None => (
            Utc.from_utc_datetime(&(camera_time + Duration::hours(7))),
            Utc.from_utc_datetime(&(camera_time + Duration::hours(8))),
        ),
    }
}

/// The camera clock time behind a `DateTimeOriginal` SmugMug returned.
pub fn camera_time(stored: DateTime<Utc>) -> NaiveDateTime {
    stored.with_timezone(&Los_Angeles).naive_local()
}

/// Parse a date-taken bound typed by a user: `YYYY-MM-DD`, optionally
/// followed by `HH:MM` or `HH:MM:SS` (after a space or `T`), in camera clock
/// time. A missing time means the start of the day, or with `end` the last
/// second of the day (or of the minute).
pub fn parse_bound(value: &str, end: bool) -> Result<NaiveDateTime> {
    let value = value.trim();
    let normalized = value.replacen('T', " ", 1);
    if let Ok(t) = NaiveDateTime::parse_from_str(&normalized, "%Y-%m-%d %H:%M:%S") {
        return Ok(t);
    }
    if let Ok(t) = NaiveDateTime::parse_from_str(&normalized, "%Y-%m-%d %H:%M") {
        return Ok(if end { t + Duration::seconds(59) } else { t });
    }
    if let Ok(d) = NaiveDate::parse_from_str(value, "%Y-%m-%d") {
        return Ok(if end {
            d.and_hms_opt(23, 59, 59).expect("valid time")
        } else {
            d.and_hms_opt(0, 0, 0).expect("valid time")
        });
    }
    anyhow::bail!(
        "Can't read the date \"{}\": use YYYY-MM-DD, optionally followed by HH:MM or HH:MM:SS",
        value
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn camera(s: &str) -> NaiveDateTime {
        NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S").unwrap()
    }

    fn utc(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().to_utc()
    }

    #[test]
    fn test_span_matches_what_smugmug_stored() {
        // Values SmugMug returned for probe uploads (see the module docs).
        let summer = utc("2026-05-01T19:00:00Z");
        assert_eq!(stored_span(camera("2026-05-01 12:00:00")), (summer, summer));
        let winter = utc("2026-01-15T20:00:00Z");
        assert_eq!(stored_span(camera("2026-01-15 12:00:00")), (winter, winter));
    }

    #[test]
    fn test_span_around_daylight_saving() {
        // 01:30 happens twice on 2026-11-01.
        assert_eq!(
            stored_span(camera("2026-11-01 01:30:00")),
            (utc("2026-11-01T08:30:00Z"), utc("2026-11-01T09:30:00Z"))
        );
        // 02:30 never happens on 2026-03-08.
        assert_eq!(
            stored_span(camera("2026-03-08 02:30:00")),
            (utc("2026-03-08T09:30:00Z"), utc("2026-03-08T10:30:00Z"))
        );
    }

    #[test]
    fn test_camera_time_round_trips() {
        assert_eq!(
            camera_time(utc("2026-05-01T19:00:00Z")),
            camera("2026-05-01 12:00:00")
        );
        assert_eq!(
            camera_time(utc("2026-01-15T20:00:00Z")),
            camera("2026-01-15 12:00:00")
        );
    }

    #[test]
    fn test_parse_bound() {
        assert_eq!(
            parse_bound("2026-05-01", false).unwrap(),
            camera("2026-05-01 00:00:00")
        );
        assert_eq!(
            parse_bound("2026-05-01", true).unwrap(),
            camera("2026-05-01 23:59:59")
        );
        assert_eq!(
            parse_bound("2026-05-01T12:30", false).unwrap(),
            camera("2026-05-01 12:30:00")
        );
        assert_eq!(
            parse_bound("2026-05-01 12:30", true).unwrap(),
            camera("2026-05-01 12:30:59")
        );
        assert_eq!(
            parse_bound(" 2026-05-01 12:30:15 ", true).unwrap(),
            camera("2026-05-01 12:30:15")
        );
        assert!(parse_bound("May 1", false).is_err());
        assert!(parse_bound("2026-13-01", false).is_err());
    }
}
