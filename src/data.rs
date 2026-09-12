use anyhow::{Context, Result};
use chrono::{DateTime, Datelike, Duration, Local, NaiveDate, Timelike, Utc};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::pricing::{Price, PriceBook, Pricing};

/// Token usage for a single step or aggregate. `input` is the *uncached*
/// portion of prompt tokens; `cached` is the cache-hit subset.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Usage {
    pub input: u64,
    pub cached: u64,
    pub output: u64,
}

impl Usage {
    pub fn total(&self) -> u64 {
        self.input + self.cached + self.output
    }

    pub fn add(&mut self, other: &Usage) {
        self.input += other.input;
        self.cached += other.cached;
        self.output += other.output;
    }

    pub fn cost(&self, p: &Price) -> f64 {
        (self.input as f64 * p.input
            + self.cached as f64 * p.cached
            + self.output as f64 * p.output)
            / 1_000_000.0
    }
}

#[derive(Debug)]
pub struct ModelStat {
    /// Canonical display label (rule label, or raw name if unpriced).
    pub label: String,
    /// Raw transcript model names folded into this stat.
    pub raw_names: Vec<String>,
    pub usage: Usage,
    pub sessions: usize,
    pub steps: usize,
    pub pricing: Pricing,
    /// Per-1M-token price used for the list-price estimate.
    pub price: Option<Price>,
}

impl ModelStat {
    /// Cost at the equivalent list price (the crossed-out figure for free models).
    pub fn list_cost(&self) -> Option<f64> {
        self.price.as_ref().map(|p| self.usage.cost(p))
    }
}

#[derive(Debug)]
pub struct Session {
    pub name: String,
    pub last_ts: Option<DateTime<Utc>>,
    pub usage: Usage,
    pub steps: usize,
    /// Model label -> usage within this session.
    pub models: BTreeMap<String, Usage>,
    /// List-price cost across all priced models used in the session.
    pub list_cost: f64,
    /// Actual billed cost (0 for free models).
    pub actual_cost: f64,
    /// True if any step used a model with no pricing rule.
    pub has_unpriced: bool,
}

/// Usage in one time bucket (hour / day / week).
#[derive(Debug, Default)]
pub struct Bucket {
    pub label: String,
    pub usage: Usage,
    pub list_cost: f64,
    pub actual_cost: f64,
    pub has_unpriced: bool,
}

/// How the per-bucket timeline is sliced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BucketKind {
    Hour,
    Day,
    Week,
    /// Day if the span is short, Week for long spans.
    Auto,
}

#[derive(Debug)]
pub struct Report {
    pub models: Vec<ModelStat>,
    pub sessions: Vec<Session>,
    pub buckets: Vec<Bucket>,
    pub total: Usage,
    pub total_steps: usize,
    pub list_cost: f64,
    pub actual_cost: f64,
    pub has_unpriced: bool,
    pub files_read: usize,
    pub files_failed: usize,
    pub earliest: Option<DateTime<Utc>>,
    pub latest: Option<DateTime<Utc>>,
}

#[derive(Deserialize)]
struct Transcript {
    #[serde(default)]
    steps: Vec<Step>,
}

#[derive(Deserialize)]
struct Step {
    model_name: Option<String>,
    timestamp: Option<String>,
    metrics: Option<Metrics>,
}

#[derive(Deserialize)]
struct Metrics {
    #[serde(default)]
    prompt_tokens: u64,
    #[serde(default)]
    completion_tokens: u64,
    #[serde(default)]
    cached_tokens: u64,
}

pub fn default_data_dir() -> PathBuf {
    std::env::home_dir()
        .unwrap_or_else(|| PathBuf::from("~"))
        .join(".local/share/devin/cli/transcripts")
}

/// Scan `dir` for transcript JSON files and aggregate usage, keeping only
/// steps at or after `since` (None = all time).
pub fn load(
    dir: &Path,
    book: &PriceBook,
    since: Option<DateTime<Utc>>,
    bucket: BucketKind,
) -> Result<Report> {
    let mut files: Vec<PathBuf> = fs::read_dir(dir)
        .with_context(|| format!("cannot read transcript dir {}", dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    files.sort();

    let mut report = Report {
        models: Vec::new(),
        sessions: Vec::new(),
        buckets: Vec::new(),
        total: Usage::default(),
        total_steps: 0,
        list_cost: 0.0,
        actual_cost: 0.0,
        has_unpriced: false,
        files_read: 0,
        files_failed: 0,
        earliest: None,
        latest: None,
    };

    // label -> index into report.models
    let mut model_idx: BTreeMap<String, usize> = BTreeMap::new();
    // timeline buckets: day-keyed always, hour-keyed only for Hour reports
    let mut day_buckets: BTreeMap<NaiveDate, Bucket> = BTreeMap::new();
    let mut hour_buckets: BTreeMap<String, Bucket> = BTreeMap::new();

    for path in files {
        let text = match fs::read_to_string(&path) {
            Ok(t) => t,
            Err(_) => {
                report.files_failed += 1;
                continue;
            }
        };
        let t: Transcript = match serde_json::from_str(&text) {
            Ok(t) => t,
            Err(_) => {
                report.files_failed += 1;
                continue;
            }
        };
        report.files_read += 1;

        let name = path
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        let mut session = Session {
            name,
            last_ts: None,
            usage: Usage::default(),
            steps: 0,
            models: BTreeMap::new(),
            list_cost: 0.0,
            actual_cost: 0.0,
            has_unpriced: false,
        };

        for step in &t.steps {
            let (Some(m), Some(raw_model)) = (&step.metrics, &step.model_name) else {
                continue;
            };
            if m.prompt_tokens == 0 && m.completion_tokens == 0 {
                continue;
            }
            let ts = step
                .timestamp
                .as_deref()
                .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                .map(|t| t.with_timezone(&Utc));
            if let (Some(c), Some(t)) = (since, ts)
                && t < c
            {
                continue;
            }

            let usage = Usage {
                input: m.prompt_tokens.saturating_sub(m.cached_tokens),
                cached: m.cached_tokens,
                output: m.completion_tokens,
            };
            let resolved = book.resolve(raw_model);
            let step_cost = resolved.price.as_ref().map(|p| usage.cost(p));
            let step_paid = matches!(resolved.pricing, Pricing::Paid);

            let idx = *model_idx.entry(resolved.label.clone()).or_insert_with(|| {
                report.models.push(ModelStat {
                    label: resolved.label.clone(),
                    raw_names: Vec::new(),
                    usage: Usage::default(),
                    sessions: 0,
                    steps: 0,
                    pricing: resolved.pricing.clone(),
                    price: resolved.price,
                });
                report.models.len() - 1
            });
            let stat = &mut report.models[idx];
            if !stat.raw_names.iter().any(|n| n == raw_model) {
                stat.raw_names.push(raw_model.clone());
            }
            stat.usage.add(&usage);
            stat.steps += 1;

            session
                .models
                .entry(resolved.label.clone())
                .or_default()
                .add(&usage);
            session.usage.add(&usage);
            session.steps += 1;
            session.last_ts = match (session.last_ts, ts) {
                (Some(a), Some(b)) => Some(a.max(b)),
                (None, b) => b,
                (a, None) => a,
            };
            if let Some(c) = step_cost {
                session.list_cost += c;
                if step_paid {
                    session.actual_cost += c;
                }
            } else {
                session.has_unpriced = true;
            }

            if let Some(t) = ts {
                let local = t.with_timezone(&Local);
                let day_key = local.date_naive();
                let bump = |b: &mut Bucket| {
                    b.usage.add(&usage);
                    if let Some(c) = step_cost {
                        b.list_cost += c;
                        if step_paid {
                            b.actual_cost += c;
                        }
                    } else {
                        b.has_unpriced = true;
                    }
                };
                bump(day_buckets.entry(day_key).or_default());
                if bucket == BucketKind::Hour {
                    let key = local.format("%Y-%m-%d %H").to_string();
                    bump(hour_buckets.entry(key).or_default());
                }
            }

            report.total.add(&usage);
            report.total_steps += 1;
            report.earliest = match (report.earliest, ts) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (None, b) => b,
                (a, None) => a,
            };
            report.latest = match (report.latest, ts) {
                (Some(a), Some(b)) => Some(a.max(b)),
                (None, b) => b,
                (a, None) => a,
            };
        }

        if session.steps > 0 {
            for label in session.models.keys() {
                if let Some(&i) = model_idx.get(label) {
                    report.models[i].sessions += 1;
                }
            }
            report.sessions.push(session);
        }
    }

    for m in &report.models {
        if let Some(cost) = m.list_cost() {
            report.list_cost += cost;
            if matches!(m.pricing, Pricing::Paid) {
                report.actual_cost += cost;
            }
        } else {
            report.has_unpriced = true;
        }
    }

    // resolve Auto and build the final ordered bucket list, filling gaps
    let span_days = match (report.earliest, report.latest) {
        (Some(a), Some(b)) => (b - a).num_days().max(1),
        _ => 1,
    };
    let kind = match bucket {
        BucketKind::Auto => {
            if span_days > 45 {
                BucketKind::Week
            } else {
                BucketKind::Day
            }
        }
        k => k,
    };
    report.buckets = match kind {
        BucketKind::Hour => finalize_hours(hour_buckets, since, report.latest),
        BucketKind::Day => finalize_days(day_buckets, since, report.earliest, report.latest),
        BucketKind::Week => finalize_weeks(day_buckets),
        BucketKind::Auto => unreachable!(),
    };

    report
        .models
        .sort_by_key(|m| std::cmp::Reverse(m.usage.total()));
    report
        .sessions
        .sort_by_key(|s| std::cmp::Reverse(s.last_ts));
    Ok(report)
}

fn finalize_hours(
    mut buckets: BTreeMap<String, Bucket>,
    since: Option<DateTime<Utc>>,
    latest: Option<DateTime<Utc>>,
) -> Vec<Bucket> {
    let end = latest.unwrap_or_else(Utc::now).with_timezone(&Local);
    let start = since.unwrap_or_else(|| Utc::now() - Duration::hours(23));
    let mut cur = start
        .with_timezone(&Local)
        .with_minute(0)
        .and_then(|t| t.with_second(0))
        .and_then(|t| t.with_nanosecond(0))
        .unwrap_or_else(|| start.with_timezone(&Local));
    let mut out = Vec::new();
    while cur <= end {
        let key = cur.format("%Y-%m-%d %H").to_string();
        let mut b = buckets.remove(&key).unwrap_or_default();
        b.label = cur.format("%b %d %H:00").to_string();
        out.push(b);
        cur += Duration::hours(1);
    }
    // any stragglers beyond `end` (clock skew)
    for (_, mut b) in buckets {
        if b.usage.total() > 0 {
            b.label = "?".into();
            out.push(b);
        }
    }
    // drop leading empty buckets so the timeline starts at first activity
    let first = out.iter().position(|b| b.usage.total() > 0).unwrap_or(0);
    out.drain(..first);
    out
}

fn finalize_days(
    mut buckets: BTreeMap<NaiveDate, Bucket>,
    since: Option<DateTime<Utc>>,
    earliest: Option<DateTime<Utc>>,
    latest: Option<DateTime<Utc>>,
) -> Vec<Bucket> {
    let end = latest
        .unwrap_or_else(Utc::now)
        .with_timezone(&Local)
        .date_naive();
    let start = since
        .or(earliest)
        .map(|t| t.with_timezone(&Local).date_naive())
        .unwrap_or(end);
    let mut out = Vec::new();
    let mut cur = start;
    while cur <= end {
        let mut b = buckets.remove(&cur).unwrap_or_default();
        b.label = cur.format("%b %d").to_string();
        out.push(b);
        cur += Duration::days(1);
    }
    for (_, mut b) in buckets {
        if b.usage.total() > 0 {
            b.label = "?".into();
            out.push(b);
        }
    }
    let first = out.iter().position(|b| b.usage.total() > 0).unwrap_or(0);
    out.drain(..first);
    out
}

fn finalize_weeks(days: BTreeMap<NaiveDate, Bucket>) -> Vec<Bucket> {
    let mut weeks: BTreeMap<(i32, u32), Bucket> = BTreeMap::new();
    for (d, b) in days {
        if b.usage.total() == 0 {
            continue;
        }
        let w = d.iso_week();
        weeks
            .entry((w.year(), w.week()))
            .or_default()
            .usage
            .add(&b.usage);
        let wb = weeks.get_mut(&(w.year(), w.week())).unwrap();
        wb.list_cost += b.list_cost;
        wb.actual_cost += b.actual_cost;
        wb.has_unpriced |= b.has_unpriced;
    }
    weeks
        .into_iter()
        .map(|((y, w), mut b)| {
            let mon = NaiveDate::from_isoywd_opt(y, w, chrono::Weekday::Mon);
            let sun = NaiveDate::from_isoywd_opt(y, w, chrono::Weekday::Sun);
            b.label = match (mon, sun) {
                (Some(m), Some(s)) => {
                    format!("{}–{}", m.format("%b %d"), s.format("%b %d"))
                }
                _ => format!("{y}-W{w:02}"),
            };
            b
        })
        .collect()
}
