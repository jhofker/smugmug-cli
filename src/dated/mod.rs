//! Uploading files into albums by the day they were taken:
//! `<root folder>/YYYY/MM/YYYY-MM-DD` (with `YYYY-MM-DD (2)`, ... past
//! SmugMug's per-album limit). Used by `upload` without `--album` and by
//! `backup`.
//!
//! One run: scan (stat only) → skip files the index knows are unchanged →
//! read the rest (hash, date, render RAWs) on a few blocking threads →
//! upload, link or collect on several async workers.

pub mod date;
pub mod index;
pub mod plan;
pub mod run;
pub mod scan;

use colored::Colorize;

pub use run::{RunOptions, RunStats, StopFlag, human_bytes, run};

/// Print what a run did (or, on a dry run, would do).
pub fn print_summary(stats: &RunStats, options: &RunOptions) {
    let dry = options.dry_run;
    println!();
    println!(
        "{} {} files found ({} unchanged since last time{})",
        "•".cyan(),
        stats.scanned,
        stats.unchanged,
        if stats.previously_refused > 0 {
            format!(
                ", {} of them refused by SmugMug before",
                stats.previously_refused
            )
        } else {
            String::new()
        }
    );
    if stats.unsupported > 0 {
        println!(
            "{} {} files of types SmugMug doesn't take were ignored",
            "•".cyan(),
            stats.unsupported
        );
    }
    if stats.raw_skipped_with_sibling > 0 {
        println!(
            "{} {} RAW files skipped: a JPEG or HEIC of the same name is next to them",
            "•".cyan(),
            stats.raw_skipped_with_sibling
        );
    }
    if stats.raw_skipped > 0 {
        println!(
            "{} {} RAW files skipped (raw_mode)",
            "•".cyan(),
            stats.raw_skipped
        );
    }
    if stats.live_photo_videos_skipped > 0 {
        println!(
            "{} {} Live Photo videos skipped",
            "•".cyan(),
            stats.live_photo_videos_skipped
        );
    }
    if !stats.scan_errors.is_empty() {
        println!(
            "{}",
            format!("⚠ {} paths couldn't be read:", stats.scan_errors.len()).yellow()
        );
        for e in stats.scan_errors.iter().take(10) {
            println!("    {}", e);
        }
    }

    if stats.to_process == 0 {
        println!("{} Nothing new to upload", "✓".green());
        return;
    }

    if dry {
        println!(
            "\n{} new or changed files ({}) would go into {}/YYYY/MM/YYYY-MM-DD",
            stats.to_process,
            human_bytes(stats.bytes_to_process),
            options.root_folder
        );
        print_dates(stats);
        println!(
            "\n{}",
            "Dry run: nothing was read in full or uploaded. Copies of files already on SmugMug will be linked rather than uploaded."
                .bright_black()
        );
        return;
    }

    println!();
    println!("  {}: {}", "Uploaded".green(), stats.uploaded);
    if stats.replaced > 0 {
        println!("  {}: {}", "Replaced (edited)".cyan(), stats.replaced);
    }
    if stats.linked > 0 {
        println!("  {}: {}", "Already there (copies)".cyan(), stats.linked);
    }
    if stats.collected > 0 {
        println!(
            "  {}: {}",
            "Added from other albums".cyan(),
            stats.collected
        );
    }
    if stats.touched > 0 {
        println!(
            "  {}: {}",
            "Touched, unchanged content".cyan(),
            stats.touched
        );
    }
    if stats.refused > 0 {
        println!("  {}: {}", "Refused by SmugMug".red(), stats.refused);
    }
    println!("  {}: {}", "Failed (retried next run)".red(), stats.failed);
    println!("  Data sent: {}", human_bytes(stats.bytes_uploaded));
    if stats.albums_created > 0 {
        println!("  Albums created: {}", stats.albums_created);
    }
    print_dates(stats);
    if !stats.errors.is_empty() {
        println!("\n{}", "Problems:".yellow());
        for e in &stats.errors {
            println!("    {}", e);
        }
    }
    if stats.interrupted {
        println!(
            "{}",
            "Stopped early; the rest will be done next run.".yellow()
        );
    }
}

fn print_dates(stats: &RunStats) {
    if !stats.date_sources.is_empty() {
        println!("  Dated by:");
        for (source, count) in &stats.date_sources {
            println!("    {:>8}  {}", count, source);
        }
    }
    let mut years: std::collections::BTreeMap<i32, usize> = Default::default();
    for (date, count) in &stats.days {
        *years.entry(chrono::Datelike::year(date)).or_default() += count;
    }
    if years.len() > 1 {
        println!("  By year:");
        for (year, count) in &years {
            println!("    {:>8}  {}", count, year);
        }
    }
    let mut busiest: Vec<_> = stats.days.iter().collect();
    busiest.sort_by(|a, b| b.1.cmp(a.1));
    let big: Vec<_> = busiest.iter().take(5).filter(|(_, n)| **n > 500).collect();
    if !big.is_empty() {
        println!("  Busiest days:");
        for (date, count) in big {
            let note = if **count as u64 > crate::uploader::album_series::MAX_ALBUM_IMAGES {
                " (more than one album)"
            } else {
                ""
            };
            println!("    {:>8}  {}{}", count, date, note);
        }
    }
}

/// A stop flag set by Ctrl-C or SIGTERM (`docker stop`): the run finishes
/// the files it has read and saves its state, so nothing is half done. A
/// second Ctrl-C exits at once.
pub fn stop_on_signal() -> StopFlag {
    let stop: StopFlag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = stop.clone();
    tokio::spawn(async move {
        loop {
            wait_for_signal().await;
            if flag.swap(true, std::sync::atomic::Ordering::Relaxed) {
                eprintln!("Stopping now.");
                std::process::exit(130);
            }
            eprintln!(
                "{}",
                "Stopping after the uploads in progress (Ctrl-C again to quit now)...".yellow()
            );
        }
    });
    stop
}

async fn wait_for_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        match signal(SignalKind::terminate()) {
            Ok(mut term) => {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {}
                    _ = term.recv() => {}
                }
            }
            Err(_) => {
                let _ = tokio::signal::ctrl_c().await;
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}
