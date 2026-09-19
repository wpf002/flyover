use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "flyover",
    version,
    about = "Turn a codebase into a 3D map you can fly through."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Count files, lines, and bytes per language. Honors .gitignore.
    Scan {
        /// Repo root.
        path: PathBuf,
        /// Print JSON instead of a table.
        #[arg(long)]
        json: bool,
    },
    /// Build the full index (files, symbols, imports). Not implemented: M1.
    Index { path: PathBuf },
    /// Lay out an index and cut it into tiles. Not implemented: M2.
    Layout { index: PathBuf },
    /// Open a tile set in the native viewer. Not implemented: M3.
    View { tileset: PathBuf },
}

/// Exit code for subcommands that exist in the CLI but aren't built yet.
const EXIT_NOT_IMPLEMENTED: u8 = 2;

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(code) => code,
        Err(err) => {
            eprintln!("error: {err:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<ExitCode> {
    match cli.command {
        Command::Scan { path, json } => {
            let report = flyover_index::scan(&path)
                .with_context(|| format!("scanning {}", path.display()))?;
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                print_table(&report);
            }
            Ok(ExitCode::SUCCESS)
        }
        Command::Index { .. } => Ok(not_implemented("index", "M1")),
        Command::Layout { .. } => Ok(not_implemented("layout", "M2")),
        Command::View { .. } => Ok(not_implemented("view", "M3")),
    }
}

fn not_implemented(name: &str, milestone: &str) -> ExitCode {
    eprintln!("`flyover {name}` isn't implemented yet. It lands in {milestone}, see docs/SPEC.md.");
    ExitCode::from(EXIT_NOT_IMPLEMENTED)
}

fn print_table(report: &flyover_index::ScanReport) {
    let mut rows: Vec<_> = report.by_language.iter().collect();
    rows.sort_by(|a, b| b.1.lines.cmp(&a.1.lines).then_with(|| a.0.cmp(b.0)));

    println!("{:<20} {:>10} {:>14}", "language", "files", "lines");
    for (language, stats) in rows {
        println!("{:<20} {:>10} {:>14}", language, stats.files, stats.lines);
    }
    println!("{:<20} {:>10} {:>14}", "total", report.files, report.lines);
    if report.skipped_binary + report.skipped_oversize > 0 {
        println!(
            "skipped: {} binary, {} over {} MB",
            report.skipped_binary,
            report.skipped_oversize,
            flyover_index::MAX_TEXT_BYTES / (1024 * 1024)
        );
    }
}
