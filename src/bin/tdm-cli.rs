//! Headless command-line mode. It drives the same engine as the GUI and never
//! touches the UI module, so it does not link iced.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use clap::{Parser, Subcommand};
use tdm::engine::{Download, Snapshot, Status};
use tdm::model::Settings;
use tdm::native_host::filename_from_url;
use tdm::rss::{self, Rule, Seen};

#[derive(Parser)]
#[command(name = "tdm-cli", version, about = "Turbo Download Manager, headless")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Download one URL.
    Add {
        /// The URL to download.
        url: String,
        /// Folder to save into.
        #[arg(short = 'o', long = "output", default_value = ".")]
        output: PathBuf,
        /// Parallel connections. Defaults to the GUI's max segments setting.
        #[arg(short = 'c', long = "connections")]
        connections: Option<u32>,
    },
    /// Poll an RSS feed once and download each new matching item.
    Rss {
        /// The feed URL.
        feed_url: String,
        /// Text the item title must contain. Repeat the flag for more rules.
        #[arg(long = "rule", required = true)]
        rules: Vec<String>,
        /// Folder to save into.
        #[arg(short = 'o', long = "output", default_value = ".")]
        output: PathBuf,
    },
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Add {
            url,
            output,
            connections,
        } => {
            let connections = connections.unwrap_or_else(default_connections);
            download(&url, &output, connections).await.map(|path| {
                println!("saved {}", path.display());
            })
        }
        Command::Rss {
            feed_url,
            rules,
            output,
        } => rss_once(&feed_url, &rules, &output).await,
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("tdm-cli: {message}");
            ExitCode::FAILURE
        }
    }
}

/// The GUI's default connection count, so both front ends agree.
fn default_connections() -> u32 {
    Settings::default().max_segments
}

/// Download one URL into `dir` under the name the URL implies. Progress is
/// printed once a second until the run ends.
async fn download(url: &str, dir: &Path, connections: u32) -> Result<PathBuf, String> {
    tokio::fs::create_dir_all(dir)
        .await
        .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let dest = dir.join(filename_from_url(url));

    let handle = Download::new(url, dest.clone(), connections);
    let worker = handle.clone();
    let mut task = tokio::spawn(async move { worker.run().await });

    let mut tick = tokio::time::interval(Duration::from_secs(1));
    // The first tick completes at once; skip it so the first line is a second in.
    tick.tick().await;
    let joined = loop {
        tokio::select! {
            joined = &mut task => break joined,
            _ = tick.tick() => print_progress(&handle.snapshot()),
        }
    };

    let run_result = joined.map_err(|e| format!("download task failed: {e}"))?;
    if let Err(e) = run_result {
        return Err(format!("{url}: {e}"));
    }
    let snap = handle.snapshot();
    match snap.status {
        Status::Finished => Ok(dest),
        _ => Err(format!(
            "{url}: {}",
            snap.error
                .or(snap.paused_reason)
                .unwrap_or_else(|| format!("stopped with status {:?}", snap.status))
        )),
    }
}

fn print_progress(snap: &Snapshot) {
    match snap.total {
        Some(total) if total > 0 => println!(
            "{} / {} bytes ({:.1}%)",
            snap.done,
            total,
            snap.done as f64 * 100.0 / total as f64
        ),
        _ => println!("{} bytes", snap.done),
    }
}

/// Poll the feed once, then download every item the rules accept.
async fn rss_once(feed_url: &str, rule_texts: &[String], output: &Path) -> Result<(), String> {
    let rules: Vec<Rule> = rule_texts
        .iter()
        .map(|text| Rule {
            contains: text.clone(),
            episode_min: None,
        })
        .collect();
    let seen_path = rss::default_seen_path();
    let mut seen = Seen::load(&seen_path).map_err(|e| e.to_string())?;
    let found = rss::poll(feed_url, &rules, &mut seen)
        .await
        .map_err(|e| e.to_string())?;
    // Save before downloading, so items already reported are not reported again on the next run.
    seen.save(&seen_path).map_err(|e| e.to_string())?;
    println!("{} new item(s) in the feed", found.len());

    let connections = default_connections();
    let mut failures = 0usize;
    for item in &found {
        println!("{}", item.title);
        if let Err(e) = download(&item.url, output, connections).await {
            eprintln!("tdm-cli: {e}");
            failures += 1;
        }
    }
    if failures > 0 {
        Err(format!("{failures} download(s) failed"))
    } else {
        Ok(())
    }
}
