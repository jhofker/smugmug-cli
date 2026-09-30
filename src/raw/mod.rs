//! Turning RAW files into JPEGs that any SmugMug account can take.
//!
//! RAW uploads need a SmugMug Source subscription. Without one, the
//! uploader sends a JPEG instead: the full-size preview the camera embedded
//! in the RAW file, with the RAW's EXIF (capture date, camera, exposure,
//! GPS, orientation) copied in when the preview has none of its own. That
//! is the camera's own rendering (picture style, white balance), without
//! any edits made in a RAW editor, and is byte-for-byte the same each run.
//!
//! When a file has no usable preview (DNGs from Adobe DNG Converter often
//! carry only a 1024 px one), the RAW data itself is converted with
//! `rawler` (LGPL-2.1): demosaiced, white balanced and converted to sRGB,
//! with no tone curve, so it looks flatter than the camera's rendering.

use anyhow::{Context, Result, anyhow, bail};
use std::path::Path;
use std::sync::Mutex;

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

/// JPEG quality for RAW files converted with `rawler`.
const CONVERTED_JPEG_QUALITY: u8 = 92;

pub fn render_jpeg_bytes(data: &[u8]) -> Result<RenderedJpeg> {
    let mut out = match embedded_preview(data) {
        Ok(preview) if preview.has_exif => {
            return Ok(RenderedJpeg {
                data: data[preview.start..preview.end].to_vec(),
            });
        }
        Ok(preview) => data[preview.start..preview.end].to_vec(),
        Err(no_preview) => convert_raw(data)
            .map_err(|e| anyhow!("{no_preview}, and converting the RAW data failed: {e:#}"))?,
    };

    // Neither a bare preview nor a converted image has the RAW's EXIF. If it
    // can't be written, upload without it rather than not at all.
    if let Some(exif) = exif::read_raw(data).filter(|e| !e.is_empty()) {
        let mut with_exif = out.clone();
        if exif.write_into_jpeg(&mut with_exif).is_ok() {
            out = with_exif;
        }
    }

    Ok(RenderedJpeg { data: out })
}

/// The largest JPEG preview embedded in `data` (the earliest wins a tie),
/// if it's big enough to stand in for the photo.
fn embedded_preview(data: &[u8]) -> Result<jpeg::EmbeddedJpeg> {
    let Some(best) = jpeg::find_jpegs(data)
        .into_iter()
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
    Ok(best)
}

/// Decode and develop the RAW data itself with `rawler`, as a JPEG.
fn convert_raw(data: &[u8]) -> Result<Vec<u8>> {
    use image::codecs::jpeg::JpegEncoder;
    use rawler::decoders::RawDecodeParams;
    use rawler::imgop::develop::RawDevelop;
    use rawler::rawsource::RawSource;

    // A developed 45 MP image takes well over a gigabyte in floating point,
    // so conversions run one at a time however many upload workers there are.
    static CONVERTING: Mutex<()> = Mutex::new(());
    let _one_at_a_time = CONVERTING.lock().unwrap_or_else(|e| e.into_inner());

    // Decoders for unusual files can panic; that's a failed file, not a
    // failed upload run.
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let source = RawSource::new_from_slice(data);
        let raw = rawler::decode(&source, &RawDecodeParams::default())?;
        let developed = RawDevelop::default()
            .develop_intermediate(&raw)?
            .to_dynamic_image()
            .context("developed image has an unexpected size")?;

        let mut out = Vec::new();
        JpegEncoder::new_with_quality(&mut out, CONVERTED_JPEG_QUALITY)
            .encode_image(&developed.into_rgb8())?;
        Ok(out)
    }))
    .map_err(|_| anyhow!("the RAW decoder crashed on this file"))?
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

        // The result is the preview with an EXIF segment added, and it
        // parses as exactly one JPEG of the same size.
        assert!(rendered.data.ends_with(&full[2..]));
        let found = jpeg::find_jpegs(&rendered.data);
        assert_eq!(found.len(), 1);
        assert!(found[0].has_exif);
        assert_eq!((found[0].width, found[0].height), (6000, 4000));
        assert_eq!(found[0].end, rendered.data.len());

        let exif = exif::read_tiff_raw(exif::tests::exif_tiff(&rendered.data).unwrap()).unwrap();
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
    fn falls_back_to_converting_without_a_usable_preview() {
        // These fakes have no real RAW data either, so conversion fails too,
        // and the error says why neither worked.
        let mut raw = fake_tiff_raw_be();
        let err = render_jpeg_bytes(&raw).unwrap_err().to_string();
        assert!(err.contains("no embedded JPEG preview"), "{err}");
        assert!(err.contains("converting the RAW data failed"), "{err}");

        raw.extend(fake_jpeg(0xC0, 1024, 683, None));
        let err = render_jpeg_bytes(&raw).unwrap_err().to_string();
        assert!(err.contains("1024x683"), "{err}");
        assert!(err.contains("converting the RAW data failed"), "{err}");
    }
}
