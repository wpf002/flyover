use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

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
    /// Build the full index (files, symbols, imports) into `<out>/index.db`.
    Index {
        /// Repo root.
        path: PathBuf,
        /// Output directory. Writes `<out>/index.db`.
        #[arg(short, long)]
        out: PathBuf,
        /// Index vendored and generated code that is excluded by default.
        #[arg(long)]
        include_vendored: bool,
        /// Per-file parse-time budget in milliseconds. Omit for no cap (deterministic output).
        #[arg(long)]
        parse_timeout_ms: Option<u64>,
        /// Print a JSON summary instead of a table.
        #[arg(long)]
        json: bool,
    },
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
        Command::Index {
            path,
            out,
            include_vendored,
            parse_timeout_ms,
            json,
        } => {
            let options = flyover_index::Options {
                include_vendored,
                parse_timeout: parse_timeout_ms.map(Duration::from_millis),
            };
            let data = flyover_index::run(&path, &out, &options)
                .with_context(|| format!("indexing {}", path.display()))?;
            let summary = summarize(&data, &out.join("index.db"));
            if json {
                println!("{}", serde_json::to_string_pretty(&summary)?);
            } else {
                print_index_summary(&summary);
            }
            Ok(ExitCode::SUCCESS)
        }
        Command::Layout { .. } => Ok(not_implemented("layout", "M2")),
        Command::View { .. } => Ok(not_implemented("view", "M3")),
    }
}

fn not_implemented(name: &str, milestone: &str) -> ExitCode {
    eprintln!("`flyover {name}` isn't implemented yet. It lands in {milestone}, see docs/SPEC.md.");
    ExitCode::from(EXIT_NOT_IMPLEMENTED)
}

fn summarize(data: &flyover_index::IndexData, db_path: &Path) -> serde_json::Value {
    let parsed = data.files.iter().filter(|f| f.parsed).count();
    let symbols: usize = data.files.iter().map(|f| f.symbols.len()).sum();
    let imports: usize = data.files.iter().map(|f| f.imports.len()).sum();
    let lines: u64 = data.files.iter().map(|f| f.lines).sum();
    serde_json::json!({
        "db": db_path.display().to_string(),
        "files": data.files.len(),
        "parsedFiles": parsed,
        "lines": lines,
        "symbols": symbols,
        "imports": imports,
        "excluded": data.excluded.len(),
    })
}

fn print_index_summary(summary: &serde_json::Value) {
    println!("index written to {}", summary["db"].as_str().unwrap_or(""));
    for key in [
        "files",
        "parsedFiles",
        "lines",
        "symbols",
        "imports",
        "excluded",
    ] {
        println!("{key:<14} {}", summary[key]);
    }
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
