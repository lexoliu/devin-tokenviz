mod data;
mod fmt;
mod pricing;
mod ui;

use std::path::PathBuf;

use clap::Parser;

use crate::pricing::Pricing;

/// Visualize Devin CLI token usage and cost in a TUI.
#[derive(Parser)]
#[command(name = "devin-tokenviz", version, about)]
struct Args {
    /// Directory containing Devin CLI transcript JSON files.
    #[arg(long, value_name = "DIR")]
    data_dir: Option<PathBuf>,

    /// TOML file with extra [[rule]] pricing entries (takes precedence over built-ins).
    #[arg(long, value_name = "FILE")]
    pricing: Option<PathBuf>,

    /// Only include steps from the last N days.
    #[arg(long, value_name = "N")]
    days: Option<u32>,

    /// Print a summary table instead of launching the TUI.
    #[arg(long)]
    print: bool,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    let mut rules = pricing::load_default_rules();
    if let Some(p) = &args.pricing {
        rules.extend(pricing::load_rules(p)?);
    }
    let book = pricing::PriceBook::new(rules);

    let dir = args.data_dir.unwrap_or_else(data::default_data_dir);
    let report = data::load(&dir, &book, args.days)?;

    if args.print || std::env::var("TERM").is_err() {
        print_report(&report);
    } else {
        ui::run(report, dir, args.days, book)?;
    }
    Ok(())
}

fn print_report(r: &data::Report) {
    let range = match (r.earliest, r.latest) {
        (Some(a), Some(b)) => format!("{} → {}", fmt::date(a), fmt::date(b)),
        _ => "—".into(),
    };
    println!(
        "devin-tokenviz — {} sessions · {} steps · {}",
        r.sessions.len(),
        r.total_steps,
        range
    );
    println!();
    println!(
        "{:<18} {:>8} {:>9} {:>8} {:>9}  {:<22} {:>11} {:>10}",
        "MODEL", "INPUT", "CACHED", "OUTPUT", "TOTAL", "PRICED AS", "LIST", "ACTUAL"
    );
    for m in &r.models {
        let priced_as = match &m.pricing {
            Pricing::Free { billed_as } => format!("{billed_as} (free)"),
            Pricing::Paid => "list".into(),
            Pricing::Unpriced => "?".into(),
        };
        let (list, actual) = match m.list_cost() {
            Some(c) => match m.pricing {
                Pricing::Free { .. } => (format!("~~{}~~", fmt::money(c)), "$0.00".into()),
                _ => (fmt::money(c), fmt::money(c)),
            },
            None => ("?".into(), "?".into()),
        };
        println!(
            "{:<18} {:>8} {:>9} {:>8} {:>9}  {:<22} {:>11} {:>10}",
            m.label,
            fmt::tokens(m.usage.input),
            fmt::tokens(m.usage.cached),
            fmt::tokens(m.usage.output),
            fmt::tokens(m.usage.total()),
            priced_as,
            list,
            actual
        );
    }
    println!();
    println!(
        "total: {} tok (in {} · cached {} · out {})",
        fmt::tokens(r.total.total()),
        fmt::tokens(r.total.input),
        fmt::tokens(r.total.cached),
        fmt::tokens(r.total.output)
    );
    println!(
        "list (equiv.): {}   actual: {}",
        fmt::money(r.list_cost),
        fmt::money(r.actual_cost)
    );
    if r.has_unpriced {
        println!(
            "note: some models have no pricing rule — add [[rule]] entries via --pricing or ~/.config/devin-tokenviz.toml"
        );
    }
    if r.files_failed > 0 {
        println!("warning: {} transcript(s) failed to parse", r.files_failed);
    }
}
