use std::path::Path;
use walkdir::WalkDir;
use anyhow::Result;

pub fn walk_directory(path: &Path) -> Result<Vec<walkdir::DirEntry>> {
    let mut entries = Vec::new();

    for entry in WalkDir::new(path)
        .follow_links(true)
        .into_iter()
        .filter_map(|e| e.ok()) // Skip entries we can't access
    {
        // Only collect files, not directories
        if entry.file_type().is_file() {
            entries.push(entry);
        }
    }

    Ok(entries)
}
