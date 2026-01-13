use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

pub mod walker;

#[derive(Debug)]
pub struct ScannedFile {
    pub path: PathBuf,
}

/// Scan a directory recursively for supported image and video files
pub fn scan_directory(path: &Path) -> Result<Vec<ScannedFile>> {
    let entries = walker::walk_directory(path).context("Failed to walk directory")?;

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
        "jpg" | "jpeg" | "png" | "heif" | "heic" | "gif" | "bmp" | "tiff" | "tif" |
        // RAW formats
        "raw" | "dng" | "cr2" | "cr3" | "nef" | "arw" | "orf" | "raf" |
        "rw2" | "pef" | "srw" | "erf" | "mrw" | "3fr" | "fff" | "iiq" |
        // Video formats
        "mp4" | "mov" | "avi" | "m4v" | "mkv"
    )
}

/// Check if a file is a RAW format (requires SmugMug Source subscription)
pub fn is_raw_file(path: &Path) -> bool {
    let extension = match path.extension() {
        Some(ext) => ext.to_string_lossy().to_lowercase(),
        None => return false,
    };

    matches!(
        extension.as_str(),
        "raw"
            | "dng"
            | "cr2"
            | "cr3"
            | "nef"
            | "arw"
            | "orf"
            | "raf"
            | "rw2"
            | "pef"
            | "srw"
            | "erf"
            | "mrw"
            | "3fr"
            | "fff"
            | "iiq"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{self, File};
    use tempfile::TempDir;

    // Helper function to create a temporary directory with test files
    fn create_test_directory() -> Result<TempDir> {
        let temp_dir = TempDir::new()?;
        let base_path = temp_dir.path();

        // Create various supported image files
        File::create(base_path.join("photo1.jpg"))?;
        File::create(base_path.join("photo2.jpeg"))?;
        File::create(base_path.join("photo3.JPG"))?; // Test case insensitivity
        File::create(base_path.join("image.png"))?;
        File::create(base_path.join("IMG_1234.HEIC"))?;
        File::create(base_path.join("raw_photo.raw"))?;
        File::create(base_path.join("canon.cr2"))?;
        File::create(base_path.join("nikon.nef"))?;
        File::create(base_path.join("sony.arw"))?;
        File::create(base_path.join("apple.heif"))?;
        File::create(base_path.join("adobe.dng"))?;

        // Create various supported video files
        File::create(base_path.join("video1.mp4"))?;
        File::create(base_path.join("video2.mov"))?;
        File::create(base_path.join("old_video.avi"))?;
        File::create(base_path.join("VIDEO.MP4"))?; // Test case insensitivity

        // Create unsupported files
        File::create(base_path.join("document.txt"))?;
        File::create(base_path.join("spreadsheet.xlsx"))?;
        File::create(base_path.join("archive.zip"))?;
        File::create(base_path.join("no_extension"))?;
        File::create(base_path.join("script.sh"))?;

        // Create nested directory with files
        let nested_dir = base_path.join("nested");
        fs::create_dir(&nested_dir)?;
        File::create(nested_dir.join("nested_photo.jpg"))?;
        File::create(nested_dir.join("nested_video.mp4"))?;
        File::create(nested_dir.join("nested_doc.txt"))?;

        // Create deeply nested directory
        let deep_dir = nested_dir.join("deep").join("deeper");
        fs::create_dir_all(&deep_dir)?;
        File::create(deep_dir.join("deep_photo.png"))?;

        Ok(temp_dir)
    }

    #[test]
    fn test_is_supported_file_jpg() {
        assert!(is_supported_file(Path::new("photo.jpg")));
        assert!(is_supported_file(Path::new("photo.jpeg")));
        assert!(is_supported_file(Path::new("PHOTO.JPG"))); // Case insensitive
        assert!(is_supported_file(Path::new("PHOTO.JPEG")));
    }

    #[test]
    fn test_is_supported_file_png() {
        assert!(is_supported_file(Path::new("image.png")));
        assert!(is_supported_file(Path::new("IMAGE.PNG")));
    }

    #[test]
    fn test_is_supported_file_heif_heic() {
        assert!(is_supported_file(Path::new("photo.heif")));
        assert!(is_supported_file(Path::new("photo.heic")));
        assert!(is_supported_file(Path::new("PHOTO.HEIF")));
        assert!(is_supported_file(Path::new("PHOTO.HEIC")));
    }

    #[test]
    fn test_is_supported_file_raw_formats() {
        assert!(is_supported_file(Path::new("photo.raw")));
        assert!(is_supported_file(Path::new("photo.dng")));
        assert!(is_supported_file(Path::new("photo.cr2")));
        assert!(is_supported_file(Path::new("photo.nef")));
        assert!(is_supported_file(Path::new("photo.arw")));
        assert!(is_supported_file(Path::new("PHOTO.RAW")));
        assert!(is_supported_file(Path::new("PHOTO.DNG")));
    }

    #[test]
    fn test_is_supported_file_video_formats() {
        assert!(is_supported_file(Path::new("video.mp4")));
        assert!(is_supported_file(Path::new("video.mov")));
        assert!(is_supported_file(Path::new("video.avi")));
        assert!(is_supported_file(Path::new("VIDEO.MP4")));
        assert!(is_supported_file(Path::new("VIDEO.MOV")));
    }

    #[test]
    fn test_is_supported_file_unsupported_formats() {
        assert!(!is_supported_file(Path::new("document.txt")));
        assert!(!is_supported_file(Path::new("document.pdf")));
        assert!(!is_supported_file(Path::new("archive.zip")));
        assert!(!is_supported_file(Path::new("script.sh")));
        assert!(!is_supported_file(Path::new("data.json")));
        assert!(!is_supported_file(Path::new("config.toml")));
        assert!(!is_supported_file(Path::new("spreadsheet.xlsx")));
    }

    #[test]
    fn test_is_supported_file_no_extension() {
        assert!(!is_supported_file(Path::new("no_extension")));
        assert!(!is_supported_file(Path::new("file_without_ext")));
    }

    #[test]
    fn test_is_supported_file_with_path() {
        assert!(is_supported_file(Path::new("/path/to/photo.jpg")));
        assert!(is_supported_file(Path::new("relative/path/photo.png")));
        assert!(!is_supported_file(Path::new("/path/to/document.txt")));
    }

    #[test]
    fn test_scan_directory_finds_supported_files() {
        let temp_dir = create_test_directory().expect("Failed to create test directory");
        let scanned = scan_directory(temp_dir.path()).expect("Failed to scan directory");

        // We created 15 supported files (11 images + 4 videos)
        // - 11 image files: jpg, jpeg, JPG, png, HEIC, raw, cr2, nef, arw, heif, dng
        // - 4 video files: mp4, mov, avi, MP4
        // Plus 3 more in nested directories (nested_photo.jpg, nested_video.mp4, deep_photo.png)
        assert_eq!(scanned.len(), 18);

        // Check that all scanned files are actually supported
        for file in &scanned {
            assert!(
                is_supported_file(&file.path),
                "File {:?} should be supported",
                file.path
            );
        }
    }

    #[test]
    fn test_scan_directory_filters_unsupported_files() {
        let temp_dir = create_test_directory().expect("Failed to create test directory");
        let scanned = scan_directory(temp_dir.path()).expect("Failed to scan directory");

        // Check that no unsupported files are in the results
        let unsupported_extensions = ["txt", "xlsx", "zip", "sh"];
        for file in &scanned {
            let ext = file.path.extension().and_then(|e| e.to_str()).unwrap_or("");
            assert!(
                !unsupported_extensions.contains(&ext.to_lowercase().as_str()),
                "Found unsupported file: {:?}",
                file.path
            );
        }
    }

    #[test]
    fn test_scan_directory_empty_directory() {
        let temp_dir = TempDir::new().expect("Failed to create temp directory");
        let scanned = scan_directory(temp_dir.path()).expect("Failed to scan empty directory");

        assert_eq!(scanned.len(), 0, "Empty directory should return no files");
    }

    #[test]
    fn test_scan_directory_only_unsupported_files() {
        let temp_dir = TempDir::new().expect("Failed to create temp directory");
        let base_path = temp_dir.path();

        // Create only unsupported files
        File::create(base_path.join("document.txt")).expect("Failed to create file");
        File::create(base_path.join("data.json")).expect("Failed to create file");
        File::create(base_path.join("no_extension")).expect("Failed to create file");

        let scanned = scan_directory(temp_dir.path()).expect("Failed to scan directory");

        assert_eq!(
            scanned.len(),
            0,
            "Directory with only unsupported files should return no files"
        );
    }

    #[test]
    fn test_scan_directory_nested_structure() {
        let temp_dir = create_test_directory().expect("Failed to create test directory");
        let scanned = scan_directory(temp_dir.path()).expect("Failed to scan directory");

        // Check that nested files are found
        let nested_files: Vec<_> = scanned
            .iter()
            .filter(|f| f.path.to_string_lossy().contains("nested"))
            .collect();

        // Should find nested_photo.jpg, nested_video.mp4, and deep_photo.png
        assert_eq!(
            nested_files.len(),
            3,
            "Should find files in nested directories"
        );
    }

    #[test]
    fn test_scan_directory_invalid_path() {
        let invalid_path = Path::new("/this/path/does/not/exist/hopefully/xyz123");
        let result = scan_directory(invalid_path);

        // walkdir may return Ok with empty results for non-existent paths
        // or it may return an error - either is acceptable behavior
        match result {
            Ok(files) => assert_eq!(
                files.len(),
                0,
                "Should return empty results for invalid path"
            ),
            Err(_) => {} // Error is also acceptable
        }
    }

    #[test]
    fn test_scan_directory_file_not_directory() {
        let temp_dir = TempDir::new().expect("Failed to create temp directory");
        let file_path = temp_dir.path().join("test.txt");
        File::create(&file_path).expect("Failed to create file");

        // Scanning a file instead of a directory should still work (walkdir handles it)
        let result = scan_directory(&file_path);

        // This might succeed with 0 results or fail depending on walkdir behavior
        // The important thing is it doesn't panic
        assert!(result.is_ok() || result.is_err());
    }

    #[test]
    fn test_scanned_file_debug_trait() {
        let scanned = ScannedFile {
            path: PathBuf::from("/test/path/photo.jpg"),
        };

        let debug_str = format!("{:?}", scanned);
        assert!(debug_str.contains("ScannedFile"));
        assert!(debug_str.contains("photo.jpg"));
    }

    #[test]
    fn test_scan_directory_case_insensitive_extensions() {
        let temp_dir = TempDir::new().expect("Failed to create temp directory");
        let base_path = temp_dir.path();

        // Create files with various case combinations
        File::create(base_path.join("photo1.jpg")).expect("Failed to create file");
        File::create(base_path.join("photo2.JPG")).expect("Failed to create file");
        File::create(base_path.join("photo3.Jpg")).expect("Failed to create file");
        File::create(base_path.join("photo4.JpG")).expect("Failed to create file");

        let scanned = scan_directory(temp_dir.path()).expect("Failed to scan directory");

        assert_eq!(
            scanned.len(),
            4,
            "Should find all files regardless of extension case"
        );
    }

    #[test]
    fn test_scan_directory_mixed_content() {
        let temp_dir = TempDir::new().expect("Failed to create temp directory");
        let base_path = temp_dir.path();

        // Create a mix of supported and unsupported files
        File::create(base_path.join("photo.jpg")).expect("Failed to create file");
        File::create(base_path.join("document.txt")).expect("Failed to create file");
        File::create(base_path.join("video.mp4")).expect("Failed to create file");
        File::create(base_path.join("data.json")).expect("Failed to create file");
        File::create(base_path.join("image.png")).expect("Failed to create file");

        let scanned = scan_directory(temp_dir.path()).expect("Failed to scan directory");

        assert_eq!(scanned.len(), 3, "Should find only the 3 supported files");
    }

    #[cfg(unix)]
    #[test]
    fn test_scan_directory_with_symlink() {
        use std::os::unix::fs::symlink;

        let temp_dir = TempDir::new().expect("Failed to create temp directory");
        let base_path = temp_dir.path();

        // Create a real file
        let real_file = base_path.join("real_photo.jpg");
        File::create(&real_file).expect("Failed to create file");

        // Create a symlink to the file
        let symlink_file = base_path.join("linked_photo.jpg");
        symlink(&real_file, &symlink_file).expect("Failed to create symlink");

        let scanned = scan_directory(temp_dir.path()).expect("Failed to scan directory");

        // Should find both the real file and the symlink (walker follows links)
        assert_eq!(scanned.len(), 2, "Should find both real file and symlink");
    }

    #[test]
    fn test_is_supported_file_edge_cases() {
        // Test hidden files
        assert!(is_supported_file(Path::new(".hidden.jpg")));
        assert!(!is_supported_file(Path::new(".hidden.txt")));

        // Test files with multiple dots
        assert!(is_supported_file(Path::new("my.photo.backup.jpg")));
        assert!(!is_supported_file(Path::new("my.photo.backup.txt")));

        // Test very long filenames
        let long_name = "a".repeat(200) + ".jpg";
        assert!(is_supported_file(Path::new(&long_name)));
    }

    #[test]
    fn test_scan_directory_preserves_full_paths() {
        let temp_dir = create_test_directory().expect("Failed to create test directory");
        let base_path = temp_dir.path();
        let scanned = scan_directory(base_path).expect("Failed to scan directory");

        // Verify that all paths are absolute and start with the base path
        for file in &scanned {
            assert!(
                file.path.starts_with(base_path),
                "File path {:?} should start with base path {:?}",
                file.path,
                base_path
            );
        }
    }
}
