mod data;
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
    let report = data::load(&dir, &book, since, bucket)?;
    print!("{}", render::render(&report, desc, Pal::detect()));
    Ok(())
}
