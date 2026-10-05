//! `smugmug-cli backup`: a dated upload (see `crate::dated`) of the
//! configured directories, repeated every `interval`, for running unattended
//! (e.g. in a container).
//!
//! Each run after the first only `stat`s files it has seen, so a run over
//! an unchanged library costs a directory walk and no SmugMug requests. The
//! next run starts `interval` after the previous one ends, so runs never
//! overlap; the cache database's lock also keeps a second process out.

use anyhow::{Context, Result, bail};
use chrono::Local;
use colored::Colorize;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use crate::api::SmugMugClient;
use crate::dated::{self, RunOptions};

/// "90s", "30m", "6h", "1d", "1h30m".
pub fn parse_interval(text: &str) -> Result<Duration> {
    let text = text.trim();
    if text.is_empty() {
        bail!("Empty interval");
    }
    let mut total = 0u64;
    let mut number = String::new();
    for c in text.chars() {
        if c.is_ascii_digit() {
            number.push(c);
            continue;
        }
        let unit = match c.to_ascii_lowercase() {
            's' => 1,
            'm' => 60,
            'h' => 3600,
            'd' => 86400,
            ' ' => continue,
            _ => bail!("Invalid interval '{}': use e.g. 30m, 6h or 1d", text),
        };
        let n: u64 = number
            .parse()
            .with_context(|| format!("Invalid interval '{}'", text))?;
        total += n * unit;
        number.clear();
    }
    if !number.is_empty() {
        bail!("Invalid interval '{}': missing a unit (s, m, h or d)", text);
    }
    if total == 0 {
        bail!("The interval must be more than zero");
    }
    Ok(Duration::from_secs(total))
}

fn describe(duration: Duration) -> String {
    let secs = duration.as_secs();
    match secs {
        s if s % 86400 == 0 => format!("{}d", s / 86400),
        s if s % 3600 == 0 => format!("{}h", s / 3600),
        s if s % 60 == 0 => format!("{}m", s / 60),
        s => format!("{}s", s),
    }
}

pub async fn run(
    client: Arc<SmugMugClient>,
    options: RunOptions,
    interval: Option<Duration>,
) -> Result<()> {
    let stop = dated::stop_on_signal();
    println!("Backing up:");
    for source in &options.sources {
        println!("  {}", source.display());
    }
    println!(
        "Into: {}/YYYY/MM/YYYY-MM-DD (private albums by the day each file was taken)",
        options.root_folder
    );
    if !options.excludes.is_empty() {
        println!("Excluding: {}", options.excludes.join(", "));
    }
    match interval {
        Some(i) if !options.dry_run => println!("Every {} (after each run ends)", describe(i)),
        _ => {}
    }

    loop {
        let started = Local::now();
        println!(
            "\n{} {}",
            "Backup started".bold(),
            started.format("%Y-%m-%d %H:%M:%S")
        );
        match dated::run(client.clone(), &options, &stop).await {
            Ok(stats) => {
                dated::print_summary(&stats, &options);
                println!("  Duration: {}", format_secs(stats.duration_secs));
                if !options.dry_run {
                    write_last_run(&options, &stats, started);
                }
            }
            // A run that can't start (SmugMug unreachable, a source gone) is
            // reported and tried again next interval, not fatal.
            Err(e) if interval.is_some() && !options.dry_run => {
                eprintln!("{}", format!("✗ Backup failed: {:#}", e).red());
            }
            Err(e) => return Err(e),
        }

        let Some(interval) = interval.filter(|_| !options.dry_run) else {
            return Ok(());
        };
        if stop.load(Ordering::Relaxed) {
            return Ok(());
        }
        let next = Local::now() + chrono::Duration::from_std(interval)?;
        println!("Next run at {}", next.format("%Y-%m-%d %H:%M:%S"));
        // Sleep, waking every few seconds to notice a stop request.
        let deadline = tokio::time::Instant::now() + interval;
        while tokio::time::Instant::now() < deadline {
            if stop.load(Ordering::Relaxed) {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_secs(2).min(deadline - tokio::time::Instant::now()))
                .await;
        }
    }
}

fn format_secs(secs: u64) -> String {
    let (h, m, s) = (secs / 3600, secs / 60 % 60, secs % 60);
    if h > 0 {
        format!("{}h {}m {}s", h, m, s)
    } else if m > 0 {
        format!("{}m {}s", m, s)
    } else {
        format!("{}s", s)
    }
}

/// `last_run.json` in the cache directory: the latest run's numbers, for a
/// quick look (or a health check) without reading logs.
fn write_last_run(options: &RunOptions, stats: &dated::RunStats, started: chrono::DateTime<Local>) {
    #[derive(serde::Serialize)]
    struct LastRun<'a> {
        started: String,
        finished: String,
        sources: &'a [std::path::PathBuf],
        folder: &'a str,
        #[serde(flatten)]
        stats: &'a dated::RunStats,
    }
    let last = LastRun {
        started: started.to_rfc3339(),
        finished: Local::now().to_rfc3339(),
        sources: &options.sources,
        folder: &options.root_folder,
        stats,
    };
    let path = options.cache_path.join("last_run.json");
    match serde_json::to_vec_pretty(&last) {
        Ok(json) => {
            if let Err(e) = std::fs::write(&path, json) {
                eprintln!("Couldn't write {}: {}", path.display(), e);
            }
        }
        Err(e) => eprintln!("Couldn't save the run summary: {}", e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intervals() {
        assert_eq!(parse_interval("90s").unwrap(), Duration::from_secs(90));
        assert_eq!(parse_interval("30m").unwrap(), Duration::from_secs(1800));
        assert_eq!(parse_interval("6h").unwrap(), Duration::from_secs(6 * 3600));
        assert_eq!(parse_interval("1d").unwrap(), Duration::from_secs(86400));
        assert_eq!(parse_interval("1h30m").unwrap(), Duration::from_secs(5400));
        assert_eq!(parse_interval(" 2H ").unwrap(), Duration::from_secs(7200));
        assert!(parse_interval("6").is_err());
        assert!(parse_interval("0h").is_err());
        assert!(parse_interval("six hours").is_err());
        assert!(parse_interval("").is_err());
    }

    #[test]
    fn descriptions() {
        assert_eq!(describe(Duration::from_secs(86400)), "1d");
        assert_eq!(describe(Duration::from_secs(5400)), "90m");
        assert_eq!(describe(Duration::from_secs(21600)), "6h");
        assert_eq!(describe(Duration::from_secs(45)), "45s");
    }
}
