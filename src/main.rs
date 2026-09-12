mod data;
mod db;
mod fmt;
mod pricing;
mod render;

use std::path::PathBuf;

use chrono::{Duration, Utc};
use clap::{Parser, Subcommand};

use crate::data::BucketKind;
use crate::render::Pal;

/// Visualize Devin CLI token usage and cost.
#[derive(Parser)]
#[command(name = "devin-tokenviz", version, about)]
struct Args {
    #[command(subcommand)]
    cmd: Option<Cmd>,

    /// Directory containing Devin CLI transcript JSON files.
    #[arg(long, value_name = "DIR", global = true)]
    data_dir: Option<PathBuf>,

    /// TOML file with extra [[rule]] pricing entries (takes precedence over built-ins).
    #[arg(long, value_name = "FILE", global = true)]
    pricing: Option<PathBuf>,

    /// Devin sessions.db path (used to recover calls missing from transcripts).
    #[arg(long, value_name = "FILE", global = true)]
    db: Option<PathBuf>,

    /// Only count transcript files; do not read sessions.db.
    #[arg(long, global = true)]
    transcripts_only: bool,
}

#[derive(Subcommand)]
enum Cmd {
    /// Last 24 hours, per-hour timeline.
    #[command(visible_alias = "24h")]
    Day,
    /// Last 7 days, per-day timeline.
    Week,
    /// Last 30 days, per-day timeline.
    Month,
    /// All recorded history (default).
    All,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let now = Utc::now();
    let (desc, since, bucket) = match args.cmd.unwrap_or(Cmd::All) {
        Cmd::Day => (
            "last 24h",
            Some(now - Duration::hours(24)),
            BucketKind::Hour,
        ),
        Cmd::Week => (
            "last 7 days",
            Some(now - Duration::days(7)),
            BucketKind::Day,
        ),
        Cmd::Month => (
            "last 30 days",
            Some(now - Duration::days(30)),
            BucketKind::Day,
        ),
        Cmd::All => ("all time", None, BucketKind::Auto),
    };

    let mut rules = pricing::load_default_rules();
    if let Some(p) = &args.pricing {
        rules.extend(pricing::load_rules(p)?);
    }
    let book = pricing::PriceBook::new(rules);

    let dir = args.data_dir.unwrap_or_else(data::default_data_dir);
    let db_path = if args.transcripts_only {
        None
    } else {
        Some(args.db.clone().unwrap_or_else(|| {
            dir.parent()
                .map(|p| p.join("sessions.db"))
                .unwrap_or_else(db::default_db_path)
        }))
    };
    let report = data::load(&dir, db_path.as_deref(), &book, since, bucket)?;
    print!("{}", render::render(&report, desc, Pal::detect()));
    Ok(())
}
