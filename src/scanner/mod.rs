use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

pub mod walker;

#[derive(Debug)]
pub struct ScannedFile {
    pub path: PathBuf,
}

/// Scan a directory recursively for supported image and video files
pub fn scan_directory(path: &Path) -> Result<Vec<ScannedFile>> {
    let entries = walker::walk_directory(path)
        .context("Failed to walk directory")?;

    let mut scanned_files = Vec::new();

    for entry in entries {
        let file_path = entry.path();

        // Skip files that aren't supported
        if !is_supported_file(file_path) {
            continue;
        }

        scanned_files.push(ScannedFile {
            path: file_path.to_path_buf(),
        });
    }

    Ok(scanned_files)
}

/// Check if a file has a supported extension
pub fn is_supported_file(path: &Path) -> bool {
    let extension = match path.extension() {
        Some(ext) => ext.to_string_lossy().to_lowercase(),
        None => return false,
    };

    matches!(
        extension.as_str(),
        // Image formats
        "jpg" | "jpeg" | "png" | "heif" | "heic" | "raw" | "dng" |
        "cr2" | "nef" | "arw" |
        // Video formats
        "mp4" | "mov" | "avi"
    )
}
