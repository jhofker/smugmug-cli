//! Finding the JPEG previews cameras embed in RAW files.
//!
//! Rather than parsing each maker's container (TIFF variants, CR3's ISO
//! BMFF boxes, RAF's header, ...), this walks the file for JPEG streams and
//! validates each one's marker structure. Lossless JPEG streams (SOF3 and
//! friends) are rejected: CR2 and DNG store the sensor data itself that way,
//! and it is not a viewable image.

/// A JPEG stream found inside a larger file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbeddedJpeg {
    /// Byte range of the stream, SOI through EOI.
    pub start: usize,
    pub end: usize,
    pub width: u16,
    pub height: u16,
    /// Whether the stream has its own EXIF (APP1) segment.
    pub has_exif: bool,
}

impl EmbeddedJpeg {
    pub fn pixels(&self) -> u64 {
        self.width as u64 * self.height as u64
    }

    pub fn long_edge(&self) -> u16 {
        self.width.max(self.height)
    }
}

/// Every well-formed baseline or progressive JPEG stream in `data`, in file
/// order. Streams nested inside another one (e.g. an EXIF thumbnail inside a
/// preview's APP1 segment) are not listed separately.
pub fn find_jpegs(data: &[u8]) -> Vec<EmbeddedJpeg> {
    let mut found = Vec::new();
    let mut i = 0;
    while i + 3 < data.len() {
        let Some(offset) = data[i..].windows(3).position(|w| w == [0xFF, 0xD8, 0xFF]) else {
            break;
        };
        let start = i + offset;
        match parse_jpeg(data, start) {
            Some(jpeg) => {
                i = jpeg.end;
                found.push(jpeg);
            }
            None => i = start + 1,
        }
    }
    found
}

fn u16_be(data: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes([*data.get(at)?, *data.get(at + 1)?]))
}

/// Validate the JPEG stream starting (with SOI) at `start` and find its end.
fn parse_jpeg(data: &[u8], start: usize) -> Option<EmbeddedJpeg> {
    let mut p = start + 2;
    let mut size: Option<(u16, u16)> = None;
    let mut has_exif = false;

    loop {
        if *data.get(p)? != 0xFF {
            return None;
        }
        // Any number of 0xFF fill bytes may precede a marker.
        while *data.get(p + 1)? == 0xFF {
            p += 1;
        }
        let marker = *data.get(p + 1)?;
        match marker {
            // Standalone markers
            0x01 | 0xD0..=0xD7 => {
                p += 2;
                continue;
            }
            // A second SOI or an EOI before any scan: not a real stream
            0xD8 | 0xD9 => return None,
            0xC0..=0xFE => {}
            _ => return None,
        }

        let len = u16_be(data, p + 2)? as usize;
        if len < 2 {
            return None;
        }
        let segment = data.get(p + 4..p + 2 + len)?;

        match marker {
            // Baseline, extended sequential and progressive Huffman: viewable
            0xC0..=0xC2 => {
                if segment.len() < 6 || segment[0] != 8 {
                    return None;
                }
                let height = u16::from_be_bytes([segment[1], segment[2]]);
                let width = u16::from_be_bytes([segment[3], segment[4]]);
                if width == 0 || height == 0 {
                    return None;
                }
                size = Some((width, height));
            }
            // Lossless (RAW sensor data), hierarchical and arithmetic-coded
            // frames: not something to upload
            0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF => return None,
            0xE1 if segment.starts_with(b"Exif\0\0") => has_exif = true,
            _ => {}
        }

        p += 2 + len;

        if marker == 0xDA {
            // Start of scan: a frame header must have come first.
            size?;
            // Entropy-coded data runs to the next marker other than a
            // stuffed zero or a restart marker.
            loop {
                let offset = data.get(p..)?.iter().position(|&b| b == 0xFF)?;
                p += offset;
                match *data.get(p + 1)? {
                    0x00 | 0xD0..=0xD7 => p += 2,
                    0xFF => p += 1,
                    0xD9 => {
                        let (width, height) = size?;
                        return Some(EmbeddedJpeg {
                            start,
                            end: p + 2,
                            width,
                            height,
                            has_exif,
                        });
                    }
                    // Another segment (e.g. the next scan of a progressive
                    // JPEG): back to reading markers.
                    _ => break,
                }
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A minimal, structurally valid JPEG: SOI, optional APP1, DQT, SOF
    /// (`sof` marker), DHT, SOS, a few bytes of entropy data (with a stuffed
    /// 0xFF00 and a restart marker) and EOI. Not decodable, which is fine
    /// since only the structure is checked.
    pub fn fake_jpeg(sof: u8, width: u16, height: u16, exif: Option<&[u8]>) -> Vec<u8> {
        let mut j = vec![0xFF, 0xD8];
        if let Some(exif) = exif {
            j.extend([0xFF, 0xE1]);
            j.extend(((exif.len() + 2) as u16).to_be_bytes());
            j.extend(exif);
        }
        // DQT
        j.extend([0xFF, 0xDB, 0x00, 0x43, 0x00]);
        j.extend([1u8; 64]);
        // SOF
        j.extend([0xFF, sof, 0x00, 0x0B, 0x08]);
        j.extend(height.to_be_bytes());
        j.extend(width.to_be_bytes());
        j.extend([0x01, 0x01, 0x11, 0x00]);
        // DHT (empty tables are fine for a structure check)
        j.extend([0xFF, 0xC4, 0x00, 0x03, 0x00]);
        // SOS
        j.extend([0xFF, 0xDA, 0x00, 0x08, 0x01, 0x01, 0x00, 0x00, 0x3F, 0x00]);
        j.extend([0x12, 0xFF, 0x00, 0x34, 0xFF, 0xD0, 0x56]);
        j.extend([0xFF, 0xD9]);
        j
    }

    #[test]
    fn finds_streams_and_skips_noise() {
        let big = fake_jpeg(0xC0, 6000, 4000, None);
        let small = fake_jpeg(0xC2, 160, 120, None);
        let mut data = b"junk \xFF\xD8\xFF not a jpeg".to_vec();
        let big_start = data.len();
        data.extend(&big);
        data.extend(b"more junk");
        let small_start = data.len();
        data.extend(&small);
        data.extend(b"tail");

        let found = find_jpegs(&data);
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].start, big_start);
        assert_eq!(found[0].end, big_start + big.len());
        assert_eq!((found[0].width, found[0].height), (6000, 4000));
        assert_eq!(found[1].start, small_start);
        assert_eq!((found[1].width, found[1].height), (160, 120));
    }

    #[test]
    fn rejects_lossless_sensor_data() {
        let data = fake_jpeg(0xC3, 6000, 4000, None);
        assert!(find_jpegs(&data).is_empty());
    }

    #[test]
    fn nested_thumbnail_is_not_listed() {
        let thumb = fake_jpeg(0xC0, 160, 120, None);
        let mut exif = b"Exif\0\0".to_vec();
        exif.extend(&thumb);
        let data = fake_jpeg(0xC0, 1920, 1280, Some(&exif));

        let found = find_jpegs(&data);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].width, 1920);
        assert!(found[0].has_exif);
    }

    #[test]
    fn truncated_stream_is_ignored() {
        let mut data = fake_jpeg(0xC0, 1920, 1280, None);
        data.truncate(data.len() - 2);
        assert!(find_jpegs(&data).is_empty());
    }
}
