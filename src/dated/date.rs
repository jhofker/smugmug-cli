//! The day a photo or video was taken, which decides its album.
//!
//! In order of preference:
//! 1. EXIF `DateTimeOriginal` (or `DateTimeDigitized`): JPEG, HEIC, PNG,
//!    TIFF and RAW files. Camera wall-clock time, used as is.
//! 2. QuickTime/MP4 metadata: Apple's `com.apple.quicktime.creationdate`
//!    (local time), else the movie header's creation time (UTC, shown in
//!    the local time zone, i.e. `TZ`).
//! 3. A date in the file name (`IMG_20140712_…`, `2014-07-12 …`,
//!    `IMG-20140712-WA0001`, `FB_IMG_1405171391000`).
//! 4. EXIF `DateTime` (when the file was last written, often by an editor or
//!    scanner, so a weaker hint than the name).
//! 5. The file's modification time.

use chrono::{DateTime, Datelike, Local, NaiveDate, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::Path;

/// Where a file's date came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DateSource {
    #[serde(rename = "e")]
    Exif,
    #[serde(rename = "v")]
    Video,
    #[serde(rename = "n")]
    FileName,
    /// EXIF `DateTime`: when the file was last written.
    #[serde(rename = "w")]
    ExifModified,
    #[serde(rename = "m")]
    Modified,
    /// A Live Photo video, dated by its photo.
    #[serde(rename = "l")]
    LivePhoto,
}

impl DateSource {
    pub fn describe(self) -> &'static str {
        match self {
            DateSource::Exif => "photo metadata",
            DateSource::Video => "video metadata",
            DateSource::FileName => "file name",
            DateSource::ExifModified => "photo's last-edited date",
            DateSource::Modified => "file modified time",
            DateSource::LivePhoto => "Live Photo's photo",
        }
    }
}

/// `path`'s capture date. `data` is the whole file when the caller already
/// has it in memory (RAW files, which are read whole anyway); otherwise only
/// the parts holding metadata are read.
pub fn capture_date(path: &Path, data: Option<&[u8]>, mtime_ns: i64) -> (NaiveDate, DateSource) {
    let exif = exif_dates(path, data);
    if let Some(date) = exif.original {
        return (date, DateSource::Exif);
    }
    if let Some(date) = video_date(path) {
        return (date, DateSource::Video);
    }
    if let Some(date) = path
        .file_name()
        .and_then(|n| file_name_date(&n.to_string_lossy()))
    {
        return (date, DateSource::FileName);
    }
    if let Some(date) = exif.modified {
        return (date, DateSource::ExifModified);
    }
    (modified_date(mtime_ns), DateSource::Modified)
}

/// The date from metadata alone (EXIF or video), for dating a Live Photo
/// video by its photo.
pub fn metadata_date(path: &Path) -> Option<NaiveDate> {
    exif_dates(path, None).original.or_else(|| video_date(path))
}

fn modified_date(mtime_ns: i64) -> NaiveDate {
    Local
        .timestamp_opt(mtime_ns.div_euclid(1_000_000_000), 0)
        .single()
        .map(|t| t.date_naive())
        .unwrap_or_else(|| Local::now().date_naive())
}

/// A date that could be real: not before photography went digital-ish
/// (scans can be older, but their EXIF date is the scan's), and not in the
/// future (a camera with its clock unset or wrong).
fn plausible(date: NaiveDate, earliest_year: i32) -> Option<NaiveDate> {
    let latest = Local::now().date_naive() + chrono::Duration::days(1);
    (date.year() >= earliest_year && date <= latest).then_some(date)
}

#[derive(Default)]
struct ExifDates {
    original: Option<NaiveDate>,
    modified: Option<NaiveDate>,
}

/// "YYYY:MM:DD HH:MM:SS" (EXIF); the time is ignored. Some software
/// writes "-", "/" or "." between the date's parts instead of ":" (RapidRAW
/// exports "2026-09-12 16:55:47"), which is accepted too.
fn parse_exif_datetime(bytes: &[u8]) -> Option<NaiveDate> {
    let s = std::str::from_utf8(bytes.get(..10)?).ok()?;
    let sep = s.as_bytes()[4];
    if !matches!(sep, b':' | b'-' | b'/' | b'.') || s.as_bytes()[7] != sep {
        return None;
    }
    let normalized = s.replace(sep as char, ":");
    let date = NaiveDate::parse_from_str(&normalized, "%Y:%m:%d").ok()?;
    plausible(date, 1900)
}

fn exif_dates(path: &Path, data: Option<&[u8]>) -> ExifDates {
    // RAW containers kamadak-exif doesn't read (CR3) or that we already
    // hold in memory go through the RAW reader.
    if crate::scanner::is_raw_file(path) {
        let owned;
        let data = match data {
            Some(data) => data,
            None => {
                let Some(head) = read_head(path, RAW_METADATA_HEAD) else {
                    return ExifDates::default();
                };
                owned = head;
                &owned
            }
        };
        return raw_exif_dates(data).unwrap_or_default();
    }

    let parsed = match data {
        Some(data) => exif::Reader::new().read_from_container(&mut std::io::Cursor::new(data)),
        None => match File::open(path) {
            Ok(file) => exif::Reader::new().read_from_container(&mut BufReader::new(file)),
            Err(_) => return ExifDates::default(),
        },
    };
    let Ok(parsed) = parsed else {
        return ExifDates::default();
    };
    let ascii = |tag: exif::Tag| -> Option<NaiveDate> {
        let field = parsed.get_field(tag, exif::In::PRIMARY)?;
        match &field.value {
            exif::Value::Ascii(values) => parse_exif_datetime(values.first()?),
            _ => None,
        }
    };
    ExifDates {
        original: ascii(exif::Tag::DateTimeOriginal)
            .or_else(|| ascii(exif::Tag::DateTimeDigitized)),
        modified: ascii(exif::Tag::DateTime),
    }
}

/// How much of a RAW file to read for its date when it isn't in memory
/// already: its metadata sits near the start (CR3's movie header, the
/// first IFDs of TIFF-based formats).
const RAW_METADATA_HEAD: u64 = 4 * 1024 * 1024;

fn read_head(path: &Path, len: u64) -> Option<Vec<u8>> {
    let mut data = Vec::new();
    File::open(path)
        .ok()?
        .take(len)
        .read_to_end(&mut data)
        .ok()?;
    Some(data)
}

fn raw_exif_dates(data: &[u8]) -> Option<ExifDates> {
    let tags = crate::raw::exif::read_raw(data)?;
    let find = |entries: &[crate::raw::exif::Entry], tag: u16| {
        entries
            .iter()
            .find(|e| e.tag == tag)
            .and_then(|e| parse_exif_datetime(&e.value))
    };
    Some(ExifDates {
        original: find(&tags.exif, 0x9003).or_else(|| find(&tags.exif, 0x9004)),
        modified: find(&tags.ifd0, 0x0132),
    })
}

// ---------------------------------------------------------------------------
// QuickTime / MP4

/// Seconds from 1904-01-01 (QuickTime's epoch) to 1970-01-01.
const QUICKTIME_EPOCH_OFFSET: i64 = 2_082_844_800;

/// `moov` boxes bigger than this aren't metadata worth reading.
const MAX_MOOV: u64 = 64 * 1024 * 1024;

fn video_date(path: &Path) -> Option<NaiveDate> {
    let ext = path.extension()?.to_string_lossy().to_lowercase();
    if !matches!(ext.as_str(), "mov" | "mp4" | "m4v" | "3gp") {
        return None;
    }
    let mut file = File::open(path).ok()?;
    let moov = read_top_level_box(&mut file, b"moov")?;
    quicktime_date(&moov)
}

/// The date in a `moov` box's payload: Apple's local creation date if
/// present, else the movie header's.
fn quicktime_date(moov: &[u8]) -> Option<NaiveDate> {
    apple_creation_date(moov).or_else(|| {
        let mvhd = find_box(moov, b"mvhd")?;
        let version = *mvhd.first()?;
        let seconds = if version == 1 {
            u64::from_be_bytes(mvhd.get(4..12)?.try_into().ok()?) as i64
        } else {
            u32::from_be_bytes(mvhd.get(4..8)?.try_into().ok()?) as i64
        };
        let unix = seconds - QUICKTIME_EPOCH_OFFSET;
        // Zero (1904) or 1970 means the camera didn't set it.
        if unix < 365 * 24 * 3600 {
            return None;
        }
        let utc: DateTime<Utc> = Utc.timestamp_opt(unix, 0).single()?;
        plausible(utc.with_timezone(&Local).date_naive(), 1971)
    })
}

/// Walk the file's top-level boxes (seeking past the media data) and return
/// the payload of the first one of type `wanted`.
fn read_top_level_box<R: Read + Seek>(reader: &mut R, wanted: &[u8; 4]) -> Option<Vec<u8>> {
    let len = reader.seek(SeekFrom::End(0)).ok()?;
    let mut pos = 0u64;
    while pos + 8 <= len {
        reader.seek(SeekFrom::Start(pos)).ok()?;
        let mut header = [0u8; 16];
        reader.read_exact(&mut header[..8]).ok()?;
        let mut size = u32::from_be_bytes(header[..4].try_into().ok()?) as u64;
        let mut header_len = 8;
        if size == 1 {
            reader.read_exact(&mut header[8..16]).ok()?;
            size = u64::from_be_bytes(header[8..16].try_into().ok()?);
            header_len = 16;
        } else if size == 0 {
            size = len - pos;
        }
        if size < header_len {
            return None;
        }
        if &header[4..8] == wanted {
            let payload = size - header_len;
            if payload > MAX_MOOV {
                return None;
            }
            let mut buf = vec![0u8; payload as usize];
            reader.read_exact(&mut buf).ok()?;
            return Some(buf);
        }
        pos = pos.checked_add(size)?;
    }
    None
}

/// Child boxes of a box payload: (type, payload).
fn boxes(data: &[u8]) -> impl Iterator<Item = (&[u8], &[u8])> {
    let mut pos = 0usize;
    std::iter::from_fn(move || {
        let header = data.get(pos..pos + 8)?;
        let size = u32::from_be_bytes(header[..4].try_into().ok()?) as usize;
        let (start, end) = match size {
            0 => (pos + 8, data.len()),
            1 => {
                let large = u64::from_be_bytes(data.get(pos + 8..pos + 16)?.try_into().ok()?);
                (pos + 16, pos.checked_add(large as usize)?)
            }
            n if n < 8 => return None,
            n => (pos + 8, pos.checked_add(n)?),
        };
        let payload = data.get(start..end)?;
        let typ = &header[4..8];
        pos = end;
        Some((typ, payload))
    })
}

fn find_box<'a>(data: &'a [u8], typ: &[u8; 4]) -> Option<&'a [u8]> {
    boxes(data).find(|(t, _)| t == typ).map(|(_, p)| p)
}

/// `moov/meta` (QuickTime metadata: `hdlr`, `keys`, `ilst`): the value of
/// `com.apple.quicktime.creationdate`, e.g. "2014-07-12T14:23:11-0500".
fn apple_creation_date(moov: &[u8]) -> Option<NaiveDate> {
    let meta = find_box(moov, b"meta")?;
    let keys = find_box(meta, b"keys")?;
    let count = u32::from_be_bytes(keys.get(4..8)?.try_into().ok()?);
    let mut pos = 8usize;
    let mut index = None;
    for i in 1..=count {
        let size = u32::from_be_bytes(keys.get(pos..pos + 4)?.try_into().ok()?) as usize;
        let name = keys.get(pos + 8..pos.checked_add(size)?)?;
        if name == b"com.apple.quicktime.creationdate" {
            index = Some(i);
            break;
        }
        pos += size.max(8);
    }
    let index = index?;
    let ilst = find_box(meta, b"ilst")?;
    let (_, item) =
        boxes(ilst).find(|(t, _)| u32::from_be_bytes((*t).try_into().unwrap()) == index)?;
    let data = find_box(item, b"data")?;
    // type indicator (4) + locale (4), then the UTF-8 value
    let value = std::str::from_utf8(data.get(8..)?).ok()?;
    let date = NaiveDate::parse_from_str(value.get(..10)?, "%Y-%m-%d").ok()?;
    plausible(date, 1971)
}

// ---------------------------------------------------------------------------
// File names

/// A date in a file name: eight digits (`20140712`) or year, month and day
/// split by `-`, `_` or `.` (`2014-07-12`), not part of a longer number; or
/// a Unix timestamp in milliseconds or seconds (`1405171391000`), as
/// Facebook and some messengers name files.
pub fn file_name_date(name: &str) -> Option<NaiveDate> {
    let bytes = name.as_bytes();
    let digit_at = |i: usize| bytes.get(i).is_some_and(u8::is_ascii_digit);
    let number = |from: usize, len: usize| -> Option<u32> {
        let s = name.get(from..from + len)?;
        s.bytes()
            .all(|b| b.is_ascii_digit())
            .then(|| s.parse().ok())?
    };
    let check = |y: u32, m: u32, d: u32| {
        NaiveDate::from_ymd_opt(y as i32, m, d).and_then(|date| plausible(date, 1990))
    };

    for start in 0..bytes.len() {
        if !digit_at(start) || (start > 0 && digit_at(start - 1)) {
            continue;
        }
        let run = (start..bytes.len()).take_while(|&i| digit_at(i)).count();

        // 20140712
        if run == 8
            && let (Some(y), Some(m), Some(d)) =
                (number(start, 4), number(start + 4, 2), number(start + 6, 2))
            && let Some(date) = check(y, m, d)
        {
            return Some(date);
        }
        // 2014-07-12, 2014_07_12, 2014.07.12
        if run == 4 {
            let sep = bytes.get(start + 4).copied();
            if matches!(sep, Some(b'-' | b'_' | b'.'))
                && bytes.get(start + 7).copied() == sep
                && !digit_at(start + 10)
                && let (Some(y), Some(m), Some(d)) =
                    (number(start, 4), number(start + 5, 2), number(start + 8, 2))
                && let Some(date) = check(y, m, d)
            {
                return Some(date);
            }
        }
        // 1405171391000 (ms) or 1405171391 (s)
        if run == 13 || run == 10 {
            let digits: i64 = name.get(start..start + run)?.parse().ok()?;
            let secs = if run == 13 { digits / 1000 } else { digits };
            if let Some(t) = Local.timestamp_opt(secs, 0).single()
                && let Some(date) = plausible(t.date_naive(), 2004)
            {
                return Some(date);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use tempfile::TempDir;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    #[test]
    fn file_names() {
        assert_eq!(
            file_name_date("IMG_20140712_142311.jpg"),
            Some(d(2014, 7, 12))
        );
        assert_eq!(
            file_name_date("PXL_20230101_101010123.jpg"),
            Some(d(2023, 1, 1))
        );
        assert_eq!(
            file_name_date("IMG-20140712-WA0001.jpg"),
            Some(d(2014, 7, 12))
        );
        assert_eq!(
            file_name_date("2014-07-12 14.23.11.jpg"),
            Some(d(2014, 7, 12))
        );
        assert_eq!(
            file_name_date("Screenshot_2019_03_04.png"),
            Some(d(2019, 3, 4))
        );
        assert_eq!(
            file_name_date("VID_20160101_000000.mp4"),
            Some(d(2016, 1, 1))
        );
        assert!(file_name_date("FB_IMG_1405171391000.jpg").is_some());
        // No date, or digits that aren't one
        assert_eq!(file_name_date("DSC01234.JPG"), None);
        assert_eq!(file_name_date("IMG_1234.HEIC"), None);
        assert_eq!(file_name_date("123456789012.jpg"), None);
        assert_eq!(file_name_date("20141399.jpg"), None);
        assert_eq!(file_name_date("19850101.jpg"), None);
        assert_eq!(file_name_date("2014-07-123.jpg"), None);
        assert_eq!(file_name_date("2014-07_12.jpg"), None);
    }

    #[test]
    fn exif_datetime_strings() {
        assert_eq!(
            parse_exif_datetime(b"2014:07:12 14:23:11\0"),
            Some(d(2014, 7, 12))
        );
        assert_eq!(parse_exif_datetime(b"0000:00:00 00:00:00"), None);
        assert_eq!(parse_exif_datetime(b"    :  :     :  :  "), None);
        assert_eq!(parse_exif_datetime(b"2999:01:01 00:00:00"), None);
        // Nonstandard separators some software writes
        assert_eq!(
            parse_exif_datetime(b"2026-09-12 16:55:47"),
            Some(d(2026, 9, 12))
        );
        assert_eq!(
            parse_exif_datetime(b"2026/09/12 16:55:47"),
            Some(d(2026, 9, 12))
        );
        assert_eq!(parse_exif_datetime(b"2026:09-12 16:55:47"), None);
    }

    /// A minimal JPEG whose EXIF holds the given IFD0 DateTime and EXIF
    /// DateTimeOriginal.
    fn jpeg_with_dates(modified: Option<&str>, original: Option<&str>) -> Vec<u8> {
        // Little-endian TIFF: IFD0 at 8 with DateTime and an EXIF pointer;
        // EXIF IFD with DateTimeOriginal; values after.
        let mut tiff = Vec::new();
        tiff.extend_from_slice(b"II*\0");
        tiff.extend_from_slice(&8u32.to_le_bytes());
        let ifd0_entries = 1 + modified.is_some() as u16;
        let ifd0_len = 2 + 12 * ifd0_entries as u32 + 4;
        let exif_ifd_at = 8 + ifd0_len;
        let exif_len = 2 + 12 * original.is_some() as u32 + 4;
        let mut values_at = exif_ifd_at + exif_len;
        let mut values = Vec::new();

        tiff.extend_from_slice(&ifd0_entries.to_le_bytes());
        if let Some(m) = modified {
            tiff.extend_from_slice(&0x0132u16.to_le_bytes());
            tiff.extend_from_slice(&2u16.to_le_bytes());
            tiff.extend_from_slice(&20u32.to_le_bytes());
            tiff.extend_from_slice(&values_at.to_le_bytes());
            values.extend_from_slice(m.as_bytes());
            values.push(0);
            values_at += 20;
        }
        tiff.extend_from_slice(&0x8769u16.to_le_bytes());
        tiff.extend_from_slice(&4u16.to_le_bytes());
        tiff.extend_from_slice(&1u32.to_le_bytes());
        tiff.extend_from_slice(&exif_ifd_at.to_le_bytes());
        tiff.extend_from_slice(&0u32.to_le_bytes());

        tiff.extend_from_slice(&(original.is_some() as u16).to_le_bytes());
        if let Some(o) = original {
            tiff.extend_from_slice(&0x9003u16.to_le_bytes());
            tiff.extend_from_slice(&2u16.to_le_bytes());
            tiff.extend_from_slice(&20u32.to_le_bytes());
            tiff.extend_from_slice(&values_at.to_le_bytes());
            values.extend_from_slice(o.as_bytes());
            values.push(0);
        }
        tiff.extend_from_slice(&0u32.to_le_bytes());
        tiff.extend_from_slice(&values);

        let mut app1 = b"Exif\0\0".to_vec();
        app1.extend_from_slice(&tiff);
        let mut jpeg = vec![0xFF, 0xD8, 0xFF, 0xE1];
        jpeg.extend_from_slice(&((app1.len() + 2) as u16).to_be_bytes());
        jpeg.extend_from_slice(&app1);
        jpeg.extend_from_slice(&[0xFF, 0xD9]);
        jpeg
    }

    fn write(dir: &TempDir, name: &str, data: &[u8]) -> std::path::PathBuf {
        let path = dir.path().join(name);
        std::fs::write(&path, data).unwrap();
        path
    }

    #[test]
    fn exif_original_wins() {
        let dir = TempDir::new().unwrap();
        let path = write(
            &dir,
            "IMG_20200101_000000.jpg",
            &jpeg_with_dates(Some("2021:05:05 10:00:00"), Some("2014:07:12 14:23:11")),
        );
        assert_eq!(
            capture_date(&path, None, 0),
            (d(2014, 7, 12), DateSource::Exif)
        );
        assert_eq!(metadata_date(&path), Some(d(2014, 7, 12)));
    }

    #[test]
    fn file_name_beats_exif_modified_which_beats_mtime() {
        let dir = TempDir::new().unwrap();
        let jpeg = jpeg_with_dates(Some("2021:05:05 10:00:00"), None);
        let named = write(&dir, "IMG_20200101_000000.jpg", &jpeg);
        assert_eq!(
            capture_date(&named, None, 0),
            (d(2020, 1, 1), DateSource::FileName)
        );
        let unnamed = write(&dir, "scan.jpg", &jpeg);
        assert_eq!(
            capture_date(&unnamed, None, 0),
            (d(2021, 5, 5), DateSource::ExifModified)
        );
        let bare = write(&dir, "bare.png", b"not really a png");
        let mtime = Local
            .with_ymd_and_hms(2015, 3, 4, 12, 0, 0)
            .unwrap()
            .timestamp()
            * 1_000_000_000;
        assert_eq!(
            capture_date(&bare, None, mtime),
            (d(2015, 3, 4), DateSource::Modified)
        );
    }

    #[test]
    fn exif_from_memory() {
        let jpeg = jpeg_with_dates(None, Some("2014:07:12 14:23:11"));
        let (date, source) = capture_date(Path::new("x.jpg"), Some(&jpeg), 0);
        assert_eq!((date, source), (d(2014, 7, 12), DateSource::Exif));
    }

    #[test]
    fn raw_exif() {
        let raw = crate::raw::exif::tests::fake_tiff_raw_be();
        let (_, source) = capture_date(Path::new("x.cr2"), Some(&raw), 0);
        // The fake RAW carries a DateTimeOriginal.
        assert_eq!(source, DateSource::Exif);
    }

    fn mp4_box(typ: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut b = ((payload.len() + 8) as u32).to_be_bytes().to_vec();
        b.extend_from_slice(typ);
        b.extend_from_slice(payload);
        b
    }

    fn mvhd(unix: i64) -> Vec<u8> {
        let mut p = vec![0u8; 4]; // version 0, flags
        p.extend_from_slice(&((unix + QUICKTIME_EPOCH_OFFSET) as u32).to_be_bytes());
        p.extend_from_slice(&[0u8; 92]);
        mp4_box(b"mvhd", &p)
    }

    fn apple_meta(value: &str) -> Vec<u8> {
        let mut keys = vec![0u8; 4];
        keys.extend_from_slice(&2u32.to_be_bytes());
        for name in [
            "com.apple.quicktime.make",
            "com.apple.quicktime.creationdate",
        ] {
            keys.extend_from_slice(&((name.len() + 8) as u32).to_be_bytes());
            keys.extend_from_slice(b"mdta");
            keys.extend_from_slice(name.as_bytes());
        }
        let mut data = 1u32.to_be_bytes().to_vec();
        data.extend_from_slice(&0u32.to_be_bytes());
        data.extend_from_slice(value.as_bytes());
        let item = mp4_box(&2u32.to_be_bytes(), &mp4_box(b"data", &data));
        let mut meta = mp4_box(b"hdlr", &[0u8; 24]);
        meta.extend(mp4_box(b"keys", &keys));
        meta.extend(mp4_box(b"ilst", &item));
        mp4_box(b"meta", &meta)
    }

    fn movie(moov_children: &[Vec<u8>]) -> Vec<u8> {
        let mut file = mp4_box(b"ftyp", b"qt  \0\0\0\0qt  ");
        file.extend(mp4_box(b"mdat", &[0u8; 1000]));
        file.extend(mp4_box(b"moov", &moov_children.concat()));
        file
    }

    #[test]
    fn video_prefers_apple_local_date() {
        let noon_utc = Utc
            .with_ymd_and_hms(2014, 7, 12, 12, 0, 0)
            .unwrap()
            .timestamp();
        let file = movie(&[mvhd(noon_utc), apple_meta("2014-07-11T23:30:00-0500")]);
        let moov = read_top_level_box(&mut Cursor::new(&file), b"moov").unwrap();
        assert_eq!(quicktime_date(&moov), Some(d(2014, 7, 11)));
    }

    #[test]
    fn video_falls_back_to_movie_header() {
        let noon_utc = Utc
            .with_ymd_and_hms(2014, 7, 12, 12, 0, 0)
            .unwrap()
            .timestamp();
        let file = movie(&[mvhd(noon_utc)]);
        let moov = read_top_level_box(&mut Cursor::new(&file), b"moov").unwrap();
        let expected = Utc
            .timestamp_opt(noon_utc, 0)
            .unwrap()
            .with_timezone(&Local)
            .date_naive();
        assert_eq!(quicktime_date(&moov), Some(expected));

        let dir = TempDir::new().unwrap();
        let path = write(&dir, "clip.mov", &file);
        assert_eq!(capture_date(&path, None, 0), (expected, DateSource::Video));
    }

    #[test]
    fn unset_video_dates_are_ignored() {
        let file = movie(&[mvhd(-QUICKTIME_EPOCH_OFFSET)]);
        let moov = read_top_level_box(&mut Cursor::new(&file), b"moov").unwrap();
        assert_eq!(quicktime_date(&moov), None);
    }

    #[test]
    fn truncated_movies_dont_panic() {
        let file = movie(&[mvhd(1_400_000_000)]);
        for len in 0..file.len() {
            let _ = read_top_level_box(&mut Cursor::new(&file[..len]), b"moov")
                .map(|m| quicktime_date(&m));
        }
    }
}
