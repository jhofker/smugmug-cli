//! Turning RAW files into JPEGs that any SmugMug account can take.
//!
//! RAW uploads need a SmugMug Source subscription. Without one, the
//! uploader sends a JPEG instead: the full-size preview the camera embedded
//! in the RAW file, with the RAW's EXIF (capture date, camera, exposure,
//! GPS, orientation) copied in when the preview has none of its own. That
//! is the camera's own rendering (picture style, white balance), without
//! any edits made in a RAW editor, and is byte-for-byte the same each run.

use anyhow::{Result, bail};
use std::path::Path;

pub mod exif;
pub mod jpeg;

/// Previews with a shorter long edge than this are thumbnails, not photos,
/// and aren't uploaded in place of the RAW.
pub const MIN_PREVIEW_LONG_EDGE: u16 = 1600;

/// A RAW file rendered to JPEG, ready to upload.
#[derive(Debug)]
pub struct RenderedJpeg {
    pub data: Vec<u8>,
}

/// The file name a RAW file's rendered JPEG is uploaded under:
/// `IMG_1234.CR2` becomes `IMG_1234.jpg`.
pub fn rendered_file_name(path: &Path) -> String {
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "image".to_string());
    format!("{}.jpg", stem)
}

/// Render the RAW file at `path` to a JPEG (see the module docs).
pub fn render_jpeg(path: &Path) -> Result<RenderedJpeg> {
    let data = std::fs::read(path)?;
    render_jpeg_bytes(&data)
}

pub fn render_jpeg_bytes(data: &[u8]) -> Result<RenderedJpeg> {
    let previews = jpeg::find_jpegs(data);
    // Largest preview; the earliest one wins a tie.
    let Some(best) = previews
        .iter()
        .reduce(|best, p| if p.pixels() > best.pixels() { p } else { best })
    else {
        bail!("no embedded JPEG preview found");
    };
    if best.long_edge() < MIN_PREVIEW_LONG_EDGE {
        bail!(
            "embedded preview is only {}x{} (need at least {} px on the long edge)",
            best.width,
            best.height,
            MIN_PREVIEW_LONG_EDGE
        );
    }

    let preview = &data[best.start..best.end];
    let out = if best.has_exif {
        preview.to_vec()
    } else {
        match exif::read_raw(data)
            .filter(|e| !e.is_empty())
            .and_then(|e| e.to_app1_payload())
        {
            Some(payload) => exif::insert_app1(preview, &payload),
            None => preview.to_vec(),
        }
    };

    Ok(RenderedJpeg { data: out })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::raw::exif::tests::fake_tiff_raw_be;
    use crate::raw::jpeg::tests::fake_jpeg;

    #[test]
    fn test_rendered_file_name() {
        assert_eq!(
            rendered_file_name(Path::new("/a/IMG_1234.CR2")),
            "IMG_1234.jpg"
        );
        assert_eq!(rendered_file_name(Path::new("x.y.nef")), "x.y.jpg");
    }

    #[test]
    fn picks_largest_preview_and_adds_exif() {
        let mut raw = fake_tiff_raw_be();
        let thumb = fake_jpeg(0xC0, 160, 120, None);
        let full = fake_jpeg(0xC0, 6000, 4000, None);
        let sensor = fake_jpeg(0xC3, 6100, 4100, None);
        raw.extend(&thumb);
        raw.extend(&full);
        raw.extend(&sensor);

        let rendered = render_jpeg_bytes(&raw).unwrap();

        // The result is the preview with an EXIF segment right after SOI,
        // and it parses as exactly one JPEG of the same size.
        assert_eq!(&rendered.data[..4], &[0xFF, 0xD8, 0xFF, 0xE1]);
        assert!(rendered.data.ends_with(&full[2..]));
        let found = jpeg::find_jpegs(&rendered.data);
        assert_eq!(found.len(), 1);
        assert!(found[0].has_exif);
        assert_eq!((found[0].width, found[0].height), (6000, 4000));
        assert_eq!(found[0].end, rendered.data.len());

        let app1_len = u16::from_be_bytes([rendered.data[4], rendered.data[5]]) as usize;
        let payload = &rendered.data[6..4 + app1_len];
        let exif = exif::read_tiff_raw(&payload[6..]).unwrap();
        assert_eq!(exif.orientation(), Some(6));

        // Deterministic, so re-runs produce identical bytes.
        assert_eq!(render_jpeg_bytes(&raw).unwrap().data, rendered.data);
    }

    #[test]
    fn keeps_preview_exif_when_present() {
        let mut raw = fake_tiff_raw_be();
        let full = fake_jpeg(0xC0, 6000, 4000, Some(b"Exif\0\0own"));
        raw.extend(&full);

        let rendered = render_jpeg_bytes(&raw).unwrap();
        assert_eq!(rendered.data, full);
    }

    #[test]
    fn rejects_missing_or_small_previews() {
        let mut raw = fake_tiff_raw_be();
        assert!(render_jpeg_bytes(&raw).is_err());

        raw.extend(fake_jpeg(0xC0, 1024, 683, None));
        let err = render_jpeg_bytes(&raw).unwrap_err().to_string();
        assert!(err.contains("1024x683"), "{err}");
    }
}
