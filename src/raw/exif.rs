//! Reading a RAW file's EXIF and writing it into an EXIF (APP1) segment for
//! the JPEG preview, which usually has none of its own.
//!
//! Only plain values are copied: the camera/date/exposure tags of IFD0, the
//! EXIF sub-IFD minus the MakerNote (proprietary, and full of offsets that
//! would dangle once moved), and the GPS sub-IFD. IFD0's Orientation is kept
//! so viewers rotate the (unrotated) preview the way the camera was held.

/// One IFD entry with its value bytes, always in little-endian order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub tag: u16,
    pub typ: u16,
    pub count: u32,
    pub value: Vec<u8>,
}

/// The tags worth carrying from a RAW file to its rendered JPEG.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ExifData {
    pub ifd0: Vec<Entry>,
    pub exif: Vec<Entry>,
    pub gps: Vec<Entry>,
}

const TAG_EXIF_IFD: u16 = 0x8769;
const TAG_GPS_IFD: u16 = 0x8825;
const TAG_INTEROP_IFD: u16 = 0xA005;
const TAG_MAKER_NOTE: u16 = 0x927C;
const TAG_PIXEL_X_DIMENSION: u16 = 0xA002;
const TAG_PIXEL_Y_DIMENSION: u16 = 0xA003;
const TAG_ORIENTATION: u16 = 0x0112;

/// IFD0 tags copied to the JPEG. The rest of a RAW's IFD0 describes its own
/// image data (strips, compression, sensor layout) and would be wrong there.
const IFD0_TAGS: &[u16] = &[
    0x010E, // ImageDescription
    0x010F, // Make
    0x0110, // Model
    TAG_ORIENTATION,
    0x0131, // Software
    0x0132, // DateTime
    0x013B, // Artist
    0x8298, // Copyright
];

/// Values bigger than this (long UserComments and the like) are dropped so
/// the segment stays under JPEG's 64 KB segment limit.
const MAX_VALUE_LEN: usize = 4096;

const TYPE_LONG: u16 = 4;

/// Size in bytes of one element of an IFD type, and the size of the units
/// whose byte order must be swapped (rationals are pairs of 4-byte ints).
fn type_sizes(typ: u16) -> Option<(usize, usize)> {
    Some(match typ {
        1 | 2 | 6 | 7 => (1, 1),   // BYTE, ASCII, SBYTE, UNDEFINED
        3 | 8 => (2, 2),           // SHORT, SSHORT
        4 | 9 | 11 | 13 => (4, 4), // LONG, SLONG, FLOAT, IFD
        5 | 10 => (8, 4),          // RATIONAL, SRATIONAL
        12 => (8, 8),              // DOUBLE
        _ => return None,
    })
}

/// A TIFF structure (header plus IFDs) inside a byte slice.
struct Tiff<'a> {
    data: &'a [u8],
    little_endian: bool,
}

impl<'a> Tiff<'a> {
    /// Accepts standard TIFF headers and the variants RAW formats use
    /// (Olympus "IIRO"/"IIRS"/"MMOR", Panasonic "IIU\0"). Returns the TIFF
    /// and the offset of its first IFD.
    fn parse(data: &'a [u8]) -> Option<(Self, u32)> {
        let little_endian = match data.get(0..2)? {
            b"II" => true,
            b"MM" => false,
            _ => return None,
        };
        let tiff = Tiff {
            data,
            little_endian,
        };
        let magic = tiff.u16(2)?;
        if !matches!(magic, 0x2A | 0x4F52 | 0x5352 | 0x55) {
            return None;
        }
        let first_ifd = tiff.u32(4)?;
        Some((tiff, first_ifd))
    }

    fn u16(&self, at: usize) -> Option<u16> {
        let b = self.data.get(at..at + 2)?;
        Some(if self.little_endian {
            u16::from_le_bytes([b[0], b[1]])
        } else {
            u16::from_be_bytes([b[0], b[1]])
        })
    }

    fn u32(&self, at: usize) -> Option<u32> {
        let b = self.data.get(at..at + 4)?;
        Some(if self.little_endian {
            u32::from_le_bytes([b[0], b[1], b[2], b[3]])
        } else {
            u32::from_be_bytes([b[0], b[1], b[2], b[3]])
        })
    }

    /// The entries of the IFD at `offset`. Entries of unknown types or whose
    /// values fall outside the data are skipped.
    fn ifd(&self, offset: u32) -> Option<Vec<Entry>> {
        let offset = offset as usize;
        let count = self.u16(offset)? as usize;
        let mut entries = Vec::with_capacity(count);
        for i in 0..count {
            let at = offset + 2 + i * 12;
            let tag = self.u16(at)?;
            let typ = self.u16(at + 2)?;
            let n = self.u32(at + 4)?;
            let Some((elem, unit)) = type_sizes(typ) else {
                continue;
            };
            let Some(len) = (n as usize).checked_mul(elem) else {
                continue;
            };
            let value_at = if len <= 4 {
                at + 8
            } else {
                self.u32(at + 8)? as usize
            };
            let Some(bytes) = value_at
                .checked_add(len)
                .and_then(|end| self.data.get(value_at..end))
            else {
                continue;
            };
            let mut value = bytes.to_vec();
            if !self.little_endian && unit > 1 {
                for chunk in value.chunks_mut(unit) {
                    chunk.reverse();
                }
            }
            entries.push(Entry {
                tag,
                typ,
                count: n,
                value,
            });
        }
        Some(entries)
    }

    /// The offset stored in a pointer tag (e.g. the EXIF sub-IFD) of `ifd`.
    fn pointer(ifd: &[Entry], tag: u16) -> Option<u32> {
        let entry = ifd.iter().find(|e| e.tag == tag)?;
        let b = entry.value.get(0..4)?;
        Some(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
}

fn copyable(entry: &Entry) -> bool {
    entry.value.len() <= MAX_VALUE_LEN
}

/// Read EXIF from a TIFF-based RAW file (CR2, NEF, ARW, DNG, ORF, RW2, PEF,
/// ...). `None` if `data` doesn't start with a TIFF header.
pub fn read_tiff_raw(data: &[u8]) -> Option<ExifData> {
    let (tiff, ifd0_offset) = Tiff::parse(data)?;
    let ifd0 = tiff.ifd(ifd0_offset)?;

    let exif = Tiff::pointer(&ifd0, TAG_EXIF_IFD)
        .and_then(|o| tiff.ifd(o))
        .unwrap_or_default();
    let gps = Tiff::pointer(&ifd0, TAG_GPS_IFD)
        .and_then(|o| tiff.ifd(o))
        .unwrap_or_default();

    Some(ExifData {
        ifd0: ifd0
            .into_iter()
            .filter(|e| IFD0_TAGS.contains(&e.tag) && copyable(e))
            .collect(),
        exif: exif.into_iter().filter(keep_exif_entry).collect(),
        gps: gps.into_iter().filter(copyable).collect(),
    })
}

fn keep_exif_entry(entry: &Entry) -> bool {
    !matches!(
        entry.tag,
        TAG_MAKER_NOTE
            | TAG_INTEROP_IFD
            | TAG_EXIF_IFD
            | TAG_GPS_IFD
            // Describe the RAW's full size, not the preview's
            | TAG_PIXEL_X_DIMENSION
            | TAG_PIXEL_Y_DIMENSION
    ) && entry.typ != 13
        && copyable(entry)
}

/// Read EXIF from a Canon CR3 file. CR3 is an ISO BMFF container whose
/// metadata sits in boxes that each hold a complete TIFF structure: CMT1
/// (IFD0), CMT2 (EXIF sub-IFD) and CMT4 (GPS). They live in the movie
/// header near the start of the file.
pub fn read_cr3(data: &[u8]) -> Option<ExifData> {
    if data.get(4..12)? != b"ftypcrx " {
        return None;
    }
    let search = &data[..data.len().min(4 * 1024 * 1024)];
    let ifd_of = |name: &[u8; 4]| -> Option<Vec<Entry>> {
        let at = search.windows(4).position(|w| w == name)?;
        let size = u32::from_be_bytes(search.get(at.checked_sub(4)?..at)?.try_into().ok()?);
        let payload = search.get(at + 4..(at - 4).checked_add(size as usize)?)?;
        let (tiff, offset) = Tiff::parse(payload)?;
        tiff.ifd(offset)
    };

    let ifd0 = ifd_of(b"CMT1")?;
    Some(ExifData {
        ifd0: ifd0
            .into_iter()
            .filter(|e| IFD0_TAGS.contains(&e.tag) && copyable(e))
            .collect(),
        exif: ifd_of(b"CMT2")
            .unwrap_or_default()
            .into_iter()
            .filter(keep_exif_entry)
            .collect(),
        gps: ifd_of(b"CMT4")
            .unwrap_or_default()
            .into_iter()
            .filter(copyable)
            .collect(),
    })
}

/// Read whatever EXIF a RAW file's container gives access to.
pub fn read_raw(data: &[u8]) -> Option<ExifData> {
    read_tiff_raw(data).or_else(|| read_cr3(data))
}

impl ExifData {
    pub fn is_empty(&self) -> bool {
        self.ifd0.is_empty() && self.exif.is_empty() && self.gps.is_empty()
    }

    /// Serialize as an APP1 payload ("Exif\0\0" and a little-endian TIFF).
    /// `None` if it would not fit in a JPEG segment.
    pub fn to_app1_payload(&self) -> Option<Vec<u8>> {
        fn ifd_len(entries: &[Entry]) -> usize {
            let data: usize = entries
                .iter()
                .filter(|e| e.value.len() > 4)
                .map(|e| e.value.len() + e.value.len() % 2)
                .sum();
            2 + entries.len() * 12 + 4 + data
        }

        fn pointer_entry(tag: u16, offset: usize) -> Entry {
            Entry {
                tag,
                typ: TYPE_LONG,
                count: 1,
                value: (offset as u32).to_le_bytes().to_vec(),
            }
        }

        fn write_ifd(out: &mut Vec<u8>, entries: &[Entry]) {
            // `out` holds the TIFF from its header on, so its length is the
            // offset of whatever is written next.
            let mut data_at = out.len() + 2 + entries.len() * 12 + 4;
            let mut data = Vec::new();
            out.extend((entries.len() as u16).to_le_bytes());
            for e in entries {
                out.extend(e.tag.to_le_bytes());
                out.extend(e.typ.to_le_bytes());
                out.extend(e.count.to_le_bytes());
                if e.value.len() <= 4 {
                    let mut inline = e.value.clone();
                    inline.resize(4, 0);
                    out.extend(inline);
                } else {
                    out.extend((data_at as u32).to_le_bytes());
                    data.extend(&e.value);
                    if e.value.len() % 2 == 1 {
                        data.push(0);
                    }
                    data_at += e.value.len() + e.value.len() % 2;
                }
            }
            out.extend(0u32.to_le_bytes()); // no next IFD
            out.extend(data);
        }

        let mut ifd0 = self.ifd0.clone();
        let mut exif = self.exif.clone();
        let mut gps = self.gps.clone();
        for ifd in [&mut ifd0, &mut exif, &mut gps] {
            ifd.sort_by_key(|e| e.tag);
            ifd.dedup_by_key(|e| e.tag);
        }

        // Pointers take a fixed 12-byte slot, so IFD0's size is known before
        // their values are.
        let pointers = !exif.is_empty() as usize + !gps.is_empty() as usize;
        let ifd0_len = ifd_len(&ifd0) + pointers * 12;
        let exif_at = 8 + ifd0_len;
        let gps_at = exif_at + if exif.is_empty() { 0 } else { ifd_len(&exif) };
        if !exif.is_empty() {
            ifd0.push(pointer_entry(TAG_EXIF_IFD, exif_at));
        }
        if !gps.is_empty() {
            ifd0.push(pointer_entry(TAG_GPS_IFD, gps_at));
        }
        ifd0.sort_by_key(|e| e.tag);

        let mut tiff = b"II\x2A\x00\x08\x00\x00\x00".to_vec();
        write_ifd(&mut tiff, &ifd0);
        if !exif.is_empty() {
            write_ifd(&mut tiff, &exif);
        }
        if !gps.is_empty() {
            write_ifd(&mut tiff, &gps);
        }

        let mut payload = b"Exif\0\0".to_vec();
        payload.extend(tiff);
        // The segment length field (2 bytes) counts itself.
        (payload.len() + 2 <= u16::MAX as usize).then_some(payload)
    }
}

/// Insert an EXIF APP1 segment into `jpeg`: after a JFIF APP0 segment if
/// there is one (JFIF must come first), otherwise right after SOI.
pub fn insert_app1(jpeg: &[u8], payload: &[u8]) -> Vec<u8> {
    let mut at = 2;
    if jpeg.get(2..4) == Some(&[0xFF, 0xE0])
        && let Some(len) = jpeg.get(4..6)
    {
        at = (4 + u16::from_be_bytes([len[0], len[1]]) as usize).min(jpeg.len());
    }
    let mut out = Vec::with_capacity(jpeg.len() + payload.len() + 4);
    out.extend(&jpeg[..at]);
    out.extend([0xFF, 0xE1]);
    out.extend(((payload.len() + 2) as u16).to_be_bytes());
    out.extend(payload);
    out.extend(&jpeg[at..]);
    out
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    impl ExifData {
        /// The Orientation value (1-8), if present.
        pub fn orientation(&self) -> Option<u16> {
            let e = self.ifd0.iter().find(|e| e.tag == TAG_ORIENTATION)?;
            (e.typ == 3 && e.value.len() >= 2).then(|| u16::from_le_bytes([e.value[0], e.value[1]]))
        }
    }

    /// Builds a big-endian TIFF with an IFD0 (tags given), an EXIF sub-IFD
    /// and a GPS sub-IFD, like a RAW file's header.
    pub fn fake_tiff_raw_be() -> Vec<u8> {
        // Layout: header (8) | IFD0 at 8 | EXIF IFD | GPS IFD | value data
        let make = b"Canon\0";
        let date = b"2024:06:01 12:34:56\0";
        let ifd0_entries = 6; // Make, Orientation, StripOffsets, DateTime, EXIF ptr, GPS ptr
        let ifd0_at = 8;
        let exif_at = ifd0_at + 2 + ifd0_entries * 12 + 4;
        let exif_entries = 3; // ExposureTime, DateTimeOriginal, MakerNote
        let gps_at = exif_at + 2 + exif_entries * 12 + 4;
        let gps_entries = 1; // GPSLatitudeRef
        let data_at = gps_at + 2 + gps_entries * 12 + 4;

        let mut t = b"MM\x00\x2A".to_vec();
        t.extend((ifd0_at as u32).to_be_bytes());

        let mut data: Vec<u8> = Vec::new();
        let mut put = |bytes: &[u8]| -> u32 {
            let at = data_at + data.len();
            data.extend(bytes);
            at as u32
        };
        let make_at = put(make);
        let date_at = put(date);
        let exposure_at = put(&[0, 0, 0, 1, 0, 0, 0, 125]);
        let dto_at = put(date);
        let maker_note_at = put(&[0xAB; 64]);

        let entry = |t: &mut Vec<u8>, tag: u16, typ: u16, count: u32, value: [u8; 4]| {
            t.extend(tag.to_be_bytes());
            t.extend(typ.to_be_bytes());
            t.extend(count.to_be_bytes());
            t.extend(value);
        };

        t.extend((ifd0_entries as u16).to_be_bytes());
        entry(&mut t, 0x010F, 2, make.len() as u32, make_at.to_be_bytes());
        entry(&mut t, 0x0111, 4, 1, 12345u32.to_be_bytes()); // StripOffsets
        entry(&mut t, TAG_ORIENTATION, 3, 1, [0, 6, 0, 0]);
        entry(&mut t, 0x0132, 2, date.len() as u32, date_at.to_be_bytes());
        entry(&mut t, TAG_EXIF_IFD, 4, 1, (exif_at as u32).to_be_bytes());
        entry(&mut t, TAG_GPS_IFD, 4, 1, (gps_at as u32).to_be_bytes());
        t.extend(0u32.to_be_bytes());

        t.extend((exif_entries as u16).to_be_bytes());
        entry(&mut t, 0x829A, 5, 1, exposure_at.to_be_bytes());
        entry(&mut t, 0x9003, 2, date.len() as u32, dto_at.to_be_bytes());
        entry(&mut t, TAG_MAKER_NOTE, 7, 64, maker_note_at.to_be_bytes());
        t.extend(0u32.to_be_bytes());

        t.extend((gps_entries as u16).to_be_bytes());
        entry(&mut t, 0x0001, 2, 2, *b"N\0\0\0");
        t.extend(0u32.to_be_bytes());

        assert_eq!(t.len(), data_at);
        t.extend(data);
        t
    }

    #[test]
    fn reads_tiff_raw_and_filters_tags() {
        let exif = read_raw(&fake_tiff_raw_be()).unwrap();

        let ifd0_tags: Vec<u16> = exif.ifd0.iter().map(|e| e.tag).collect();
        assert_eq!(ifd0_tags, vec![0x010F, TAG_ORIENTATION, 0x0132]);
        assert_eq!(exif.orientation(), Some(6));
        assert_eq!(exif.ifd0[0].value, b"Canon\0");

        let exif_tags: Vec<u16> = exif.exif.iter().map(|e| e.tag).collect();
        assert_eq!(exif_tags, vec![0x829A, 0x9003]);
        // Rational converted to little-endian, one u32 at a time
        assert_eq!(exif.exif[0].value, vec![1, 0, 0, 0, 125, 0, 0, 0]);

        assert_eq!(exif.gps.len(), 1);
    }

    #[test]
    fn app1_round_trips_through_reader() {
        let exif = read_raw(&fake_tiff_raw_be()).unwrap();
        let payload = exif.to_app1_payload().unwrap();
        assert!(payload.starts_with(b"Exif\0\0"));

        // Our own reader must see the same tags in the written TIFF.
        let reread = read_tiff_raw(&payload[6..]).unwrap();
        assert_eq!(reread, exif);
    }

    #[test]
    fn reads_cr3_metadata_boxes() {
        // A TIFF holding just IFD0 with Model and Orientation, as in CMT1.
        let mut cmt1 = b"II\x2A\x00\x08\x00\x00\x00".to_vec();
        cmt1.extend(2u16.to_le_bytes());
        cmt1.extend(0x0110u16.to_le_bytes());
        cmt1.extend(2u16.to_le_bytes());
        cmt1.extend(4u32.to_le_bytes());
        cmt1.extend(*b"R5\0\0");
        cmt1.extend(TAG_ORIENTATION.to_le_bytes());
        cmt1.extend(3u16.to_le_bytes());
        cmt1.extend(1u32.to_le_bytes());
        cmt1.extend([8, 0, 0, 0]);
        cmt1.extend(0u32.to_le_bytes());

        let mut data = vec![0, 0, 0, 0x18];
        data.extend(b"ftypcrx ");
        data.extend([0u8; 12]);
        data.extend(((cmt1.len() + 8) as u32).to_be_bytes());
        data.extend(b"CMT1");
        data.extend(&cmt1);

        let exif = read_raw(&data).unwrap();
        assert_eq!(exif.ifd0.len(), 2);
        assert_eq!(exif.orientation(), Some(8));
        assert!(exif.exif.is_empty());
    }

    #[test]
    fn app1_goes_after_jfif() {
        let jpeg = [0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x04, 0xAA, 0xBB, 0xFF, 0xD9];
        let out = insert_app1(&jpeg, b"Exif\0\0X");
        assert_eq!(&out[..8], &jpeg[..8]);
        assert_eq!(&out[8..12], &[0xFF, 0xE1, 0x00, 0x09]);
        assert_eq!(&out[12..19], b"Exif\0\0X");
        assert_eq!(&out[19..], &[0xFF, 0xD9]);

        let bare = [0xFF, 0xD8, 0xFF, 0xDB];
        let out = insert_app1(&bare, b"E");
        assert_eq!(
            out,
            vec![0xFF, 0xD8, 0xFF, 0xE1, 0x00, 0x03, b'E', 0xFF, 0xDB]
        );
    }
}
