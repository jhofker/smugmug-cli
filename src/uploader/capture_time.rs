//! When a photo was taken, as SmugMug records it, so it can be found with an
//! exact `image!search` date range.
//!
//! SmugMug reads EXIF `DateTimeOriginal` (the camera's clock, no time zone)
//! as US Pacific time, observing daylight saving. It ignores
//! `OffsetTimeOriginal` and the account's own time zone setting. Verified
//! 2026-10-02 on an `America/Chicago` account: 12:00 on 2026-05-01 was stored
//! as 19:00Z with offset tags of -05:00, +02:00 and none, and 12:00 on
//! 2026-01-15 as 20:00Z.

use chrono::{DateTime, Duration, LocalResult, NaiveDateTime, TimeZone, Utc};
use chrono_tz::America::Los_Angeles;
use little_exif::exif_tag::ExifTag;
use little_exif::metadata::Metadata;
use std::path::Path;

const TAG_DATE_TIME_ORIGINAL: u16 = 0x9003;

/// File types whose EXIF `little_exif` reads.
const EXIF_EXTENSIONS: &[&str] = &["jpg", "jpeg", "heic", "heif", "png", "tif", "tiff", "webp"];

/// The UTC times SmugMug may record for a camera clock time: a single
/// instant, or the two candidates when daylight saving makes the clock time
/// ambiguous (the repeated hour in November) or skipped (in March).
pub fn smugmug_capture_span(camera_time: NaiveDateTime) -> (DateTime<Utc>, DateTime<Utc>) {
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

/// Parse an EXIF date ("2026:05:01 12:00:00", possibly NUL-terminated).
pub fn parse_exif_datetime(value: &str) -> Option<NaiveDateTime> {
    let value = value.trim_end_matches('\0').trim();
    NaiveDateTime::parse_from_str(value, "%Y:%m:%d %H:%M:%S").ok()
}

/// The camera clock time the file at `path` was taken, from its EXIF
/// `DateTimeOriginal`. `None` for files without one (videos, screenshots,
/// scans) or that can't be read.
pub fn read_capture_time(path: &Path) -> Option<NaiveDateTime> {
    if crate::scanner::is_raw_file(path) {
        let data = std::fs::read(path).ok()?;
        let exif = crate::raw::exif::read_raw(&data)?;
        let entry = exif.exif.iter().find(|e| e.tag == TAG_DATE_TIME_ORIGINAL)?;
        return parse_exif_datetime(&String::from_utf8_lossy(&entry.value));
    }

    let extension = path.extension()?.to_string_lossy().to_lowercase();
    if !EXIF_EXTENSIONS.contains(&extension.as_str()) {
        return None;
    }
    // A malformed file mustn't take the upload down with it.
    std::panic::catch_unwind(|| {
        let metadata = Metadata::new_from_path(path).ok()?;
        metadata
            .get_tag(&ExifTag::DateTimeOriginal(String::new()))
            .find_map(|tag| match tag {
                ExifTag::DateTimeOriginal(value) => parse_exif_datetime(value),
                _ => None,
            })
    })
    .ok()
    .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn camera(s: &str) -> NaiveDateTime {
        parse_exif_datetime(s).unwrap()
    }

    fn utc(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().to_utc()
    }

    #[test]
    fn test_parse_exif_datetime() {
        assert_eq!(
            parse_exif_datetime("2026:05:01 12:00:00\0"),
            Some(camera("2026:05:01 12:00:00"))
        );
        assert_eq!(parse_exif_datetime("0000:00:00 00:00:00"), None);
        assert_eq!(parse_exif_datetime(""), None);
    }

    #[test]
    fn test_span_matches_what_smugmug_stored() {
        // Values SmugMug returned for probe uploads (see the module docs).
        let summer = utc("2026-05-01T19:00:00Z");
        assert_eq!(
            smugmug_capture_span(camera("2026:05:01 12:00:00")),
            (summer, summer)
        );
        let winter = utc("2026-01-15T20:00:00Z");
        assert_eq!(
            smugmug_capture_span(camera("2026:01:15 12:00:00")),
            (winter, winter)
        );
        let after_spring_forward = utc("2026-03-08T10:30:00Z");
        assert_eq!(
            smugmug_capture_span(camera("2026:03:08 03:30:00")),
            (after_spring_forward, after_spring_forward)
        );
    }

    #[test]
    fn test_span_covers_both_readings_around_dst_changes() {
        // 01:30 happens twice on 2026-11-01 (PDT, then PST).
        assert_eq!(
            smugmug_capture_span(camera("2026:11:01 01:30:00")),
            (utc("2026-11-01T08:30:00Z"), utc("2026-11-01T09:30:00Z"))
        );
        // 02:30 doesn't exist on 2026-03-08.
        assert_eq!(
            smugmug_capture_span(camera("2026:03:08 02:30:00")),
            (utc("2026-03-08T09:30:00Z"), utc("2026-03-08T10:30:00Z"))
        );
    }

    #[test]
    fn test_read_capture_time_from_jpeg() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("photo.jpg");
        // A minimal JPEG with an Exif IFD holding DateTimeOriginal.
        let mut metadata = Metadata::new();
        metadata.set_tag(ExifTag::DateTimeOriginal("2026:05:01 12:00:00".into()));
        let mut jpeg = vec![0xFF, 0xD8, 0xFF, 0xD9];
        metadata
            .write_to_vec(&mut jpeg, little_exif::filetype::FileExtension::JPEG)
            .unwrap();
        std::fs::write(&path, &jpeg).unwrap();

        assert_eq!(
            read_capture_time(&path),
            Some(camera("2026:05:01 12:00:00"))
        );
    }

    #[test]
    fn test_read_capture_time_without_exif() {
        let dir = tempfile::tempdir().unwrap();
        let jpeg = dir.path().join("plain.jpg");
        std::fs::write(&jpeg, [0xFF, 0xD8, 0xFF, 0xD9]).unwrap();
        assert_eq!(read_capture_time(&jpeg), None);

        let video = dir.path().join("clip.mp4");
        std::fs::write(&video, b"not a photo").unwrap();
        assert_eq!(read_capture_time(&video), None);
    }
}
