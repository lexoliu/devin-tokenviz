use anyhow::{Context, Result};
use chrono::{DateTime, Datelike, Duration, Local, NaiveDate, Timelike, Utc};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::db;
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
    /// sessions.db was read successfully.
    pub db_used: bool,
    /// Calls recovered from sessions.db that are not in any transcript
    /// (resumed/compacted/forked chains and subagent sessions).
    pub db_recovered_calls: usize,
    /// Input tokens carried by those recovered calls.
    pub db_recovered_tokens: u64,
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

/// One model call, from either source.
struct Ev {
    session: String,
    model: String,
    ts: Option<DateTime<Utc>>,
    usage: Usage,
    /// Recovered from sessions.db (not present in any transcript).
    recovered: bool,
}

pub fn default_data_dir() -> PathBuf {
    std::env::home_dir()
        .unwrap_or_else(|| PathBuf::from("~"))
        .join(".local/share/devin/cli/transcripts")
}

/// Scan `dir` for transcript JSONs, then recover extra calls from `db_path`
/// (sessions.db) when given, and aggregate usage at or after `since`.
pub fn load(
    dir: &Path,
    db_path: Option<&Path>,
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

    let mut events: Vec<Ev> = Vec::new();
    // session -> multiset of prompt sizes, for exact dedup against db calls
    let mut prompt_ms: BTreeMap<String, BTreeMap<u64, u64>> = BTreeMap::new();
    // session -> (cached sum, prompt sum, output sum) -> share estimates
    let mut ratios: HashMap<String, (u64, u64, u64)> = HashMap::new();
    // session -> (dominant raw model, its prompt sum) as fallback model name
    let mut top_model: HashMap<String, (String, u64)> = HashMap::new();
    let mut g_cached: u64 = 0;
    let mut g_prompt: u64 = 0;
    let mut g_output: u64 = 0;
    let mut files_read = 0usize;
    let mut files_failed = 0usize;

    for path in files {
        let text = match fs::read_to_string(&path) {
            Ok(t) => t,
            Err(_) => {
                files_failed += 1;
                continue;
            }
        };
        let t: Transcript = match serde_json::from_str(&text) {
            Ok(t) => t,
            Err(_) => {
                files_failed += 1;
                continue;
            }
        };
        files_read += 1;
        let name = path
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();

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
            let cached = m.cached_tokens.min(m.prompt_tokens);
            let usage = Usage {
                input: m.prompt_tokens - cached,
                cached,
                output: m.completion_tokens,
            };
            *prompt_ms
                .entry(name.clone())
                .or_default()
                .entry(m.prompt_tokens)
                .or_default() += 1;
            let r = ratios.entry(name.clone()).or_default();
            r.0 += cached;
            r.1 += m.prompt_tokens;
            r.2 += m.completion_tokens;
            g_cached += cached;
            g_prompt += m.prompt_tokens;
            g_output += m.completion_tokens;
            let tm = top_model.entry(name.clone()).or_default();
            if m.prompt_tokens > tm.1 {
                *tm = (raw_model.clone(), m.prompt_tokens);
            }
            events.push(Ev {
                session: name.clone(),
                model: raw_model.clone(),
                ts,
                usage,
                recovered: false,
            });
        }
    }

    // ── recover calls from sessions.db ──────────────────────────────────
    let mut db_used = false;
    if let Some(p) = db_path
        && let Ok(d) = db::load(p)
    {
        db_used = true;
        let g_ratio = if g_prompt > 0 {
            (
                g_cached as f64 / g_prompt as f64,
                g_output as f64 / g_prompt as f64,
            )
        } else {
            (0.9, 0.006)
        };
        for call in d.calls {
            if call.prompt == 0 {
                continue;
            }
            // exact match against a transcript step's prompt => same call
            if let Some(ms) = prompt_ms.get_mut(&call.session)
                && let Some(n) = ms.get_mut(&call.prompt)
                && *n > 0
            {
                *n -= 1;
                continue;
            }
            // split the recovered prompt into cached/uncached at the session's
            // observed ratio, and estimate output at its observed out:in ratio
            let (cs, ps, os) = ratios.get(&call.session).copied().unwrap_or((0, 0, 0));
            let (cr, or_) = if ps > 0 {
                (cs as f64 / ps as f64, os as f64 / ps as f64)
            } else {
                g_ratio
            };
            let cached = ((call.prompt as f64 * cr).round() as u64).min(call.prompt);
            let output = (call.prompt as f64 * or_).round() as u64;
            let model = d
                .session_models
                .get(&call.session)
                .filter(|s| !s.is_empty())
                .cloned()
                .or_else(|| top_model.get(&call.session).map(|(m, _)| m.clone()))
                .unwrap_or_else(|| "unknown".to_string());
            events.push(Ev {
                session: call.session,
                model,
                ts: DateTime::from_timestamp(call.ts, 0),
                usage: Usage {
                    input: call.prompt - cached,
                    cached,
                    output,
                },
                recovered: true,
            });
        }
    }

    // ── aggregate ───────────────────────────────────────────────────────
    let mut report = Report {
        models: Vec::new(),
        sessions: Vec::new(),
        buckets: Vec::new(),
        total: Usage::default(),
        total_steps: 0,
        list_cost: 0.0,
        actual_cost: 0.0,
        has_unpriced: false,
        files_read,
        files_failed,
        earliest: None,
        latest: None,
        db_used,
        db_recovered_calls: 0,
        db_recovered_tokens: 0,
    };
    let mut model_idx: BTreeMap<String, usize> = BTreeMap::new();
    let mut sessions: BTreeMap<String, Session> = BTreeMap::new();
    let mut day_buckets: BTreeMap<NaiveDate, Bucket> = BTreeMap::new();
    let mut hour_buckets: BTreeMap<String, Bucket> = BTreeMap::new();

    for ev in events {
        if let (Some(c), Some(t)) = (since, ev.ts)
            && t < c
        {
            continue;
        }
        if ev.recovered {
            report.db_recovered_calls += 1;
            report.db_recovered_tokens += ev.usage.input + ev.usage.cached;
        }
        let usage = ev.usage;
        let resolved = book.resolve(&ev.model);
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
        if !stat.raw_names.iter().any(|n| n == &ev.model) {
            stat.raw_names.push(ev.model.clone());
        }
        stat.usage.add(&usage);
        stat.steps += 1;

        let session = sessions
            .entry(ev.session.clone())
            .or_insert_with(|| Session {
                name: ev.session.clone(),
                last_ts: None,
                usage: Usage::default(),
                steps: 0,
                models: BTreeMap::new(),
                list_cost: 0.0,
                actual_cost: 0.0,
                has_unpriced: false,
            });
        session
            .models
            .entry(resolved.label.clone())
            .or_default()
            .add(&usage);
        session.usage.add(&usage);
        session.steps += 1;
        session.last_ts = match (session.last_ts, ev.ts) {
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

        if let Some(t) = ev.ts {
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
        report.earliest = match (report.earliest, ev.ts) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (None, b) => b,
            (a, None) => a,
        };
        report.latest = match (report.latest, ev.ts) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (None, b) => b,
            (a, None) => a,
        };
    }

    for session in sessions.into_values() {
        for label in session.models.keys() {
            if let Some(&i) = model_idx.get(label) {
                report.models[i].sessions += 1;
            }
        }
        if session.steps > 0 {
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
