//! Claude Code: `~/.claude/projects/**/*.jsonl` (incl. `subagents/`).
//!
//! Each `type:"assistant"` line is one API response carrying
//! `message.model` and `message.usage`{input_tokens,
//! cache_creation_input_tokens, cache_read_input_tokens, output_tokens}.
//! Cache writes are billed as input; `cache_read` is the discounted part.
//!
//! Files are append-only: parsed entries are cached in
//! `~/.cache/llmstat/claude-files.bin` and later runs only read appended
//! tails (see `filecache`).

use anyhow::Result;
use chrono::DateTime;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use rayon::prelude::*;
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::report::Usage;
use crate::sources::SourceOut;
use crate::sources::filecache::{self, CachedCall, Dict, Entry, Plan};

#[derive(Deserialize)]
struct Line {
    #[serde(rename = "type")]
    kind: Option<String>,
    message: Option<Msg>,
    uuid: Option<String>,
    #[serde(rename = "requestId")]
    request_id: Option<String>,
    timestamp: Option<String>,
    #[serde(rename = "sessionId")]
    session_id: Option<String>,
}

#[derive(Deserialize)]
struct Msg {
    id: Option<String>,
    model: Option<String>,
    usage: Option<U>,
}

#[derive(Deserialize)]
struct U {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    cache_creation_input_tokens: u64,
    #[serde(default)]
    cache_read_input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
}

pub fn default_dir() -> PathBuf {
    std::env::home_dir()
        .unwrap_or_else(|| PathBuf::from("~"))
        .join(".claude/projects")
}

/// Parse `path` from `offset`, extending `dict` with new strings. Returns
/// (consumed bytes, dict, new tail entries).
fn parse_file(
    path: &Path,
    offset: u64,
    dict: Vec<String>,
) -> std::io::Result<(u64, Vec<String>, Vec<CachedCall>)> {
    let file_stem = path
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    let mut dict = Dict::from_vec(dict);
    let mut entries = Vec::new();
    let consumed = filecache::read_lines(
        path,
        offset,
        |s| serde_json::from_str::<Line>(s).is_ok(),
        |line| {
            if !line.contains("\"assistant\"") {
                return;
            }
            let Ok(l) = serde_json::from_str::<Line>(line) else {
                return;
            };
            if l.kind.as_deref() != Some("assistant") {
                return;
            }
            let Some(msg) = l.message else { return };
            let (Some(model), Some(u)) = (msg.model, msg.usage) else {
                return;
            };
            // "<synthetic>" placeholders and zero-usage records carry no call
            if model.starts_with('<')
                || (u.input_tokens
                    + u.cache_creation_input_tokens
                    + u.cache_read_input_tokens
                    + u.output_tokens)
                    == 0
            {
                return;
            }
            // msg.id + requestId identify an API response; the same response
            // is copied into later transcript files on resume/compact, so
            // the dedup key must not include the file name.
            let key = match (&msg.id, &l.request_id) {
                (Some(m), Some(r)) => filecache::key_of(&[m.as_bytes(), r.as_bytes()]),
                _ => filecache::key_of(&[
                    file_stem.as_bytes(),
                    l.uuid.as_deref().unwrap_or("").as_bytes(),
                ]),
            };
            entries.push(CachedCall {
                key,
                session: dict.intern(l.session_id.as_deref().unwrap_or(&file_stem)),
                model: dict.intern(&model),
                ts: l
                    .timestamp
                    .as_deref()
                    .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                    .map(|t| t.timestamp()),
                usage: Usage {
                    // cache-write tokens bill as input; cache-read is the
                    // discounted subset
                    input: u.input_tokens + u.cache_creation_input_tokens,
                    cached: u.cache_read_input_tokens,
                    output: u.output_tokens,
                },
                estimated: false,
            });
        },
    )?;
    Ok((consumed, dict.into_strings(), entries))
}

/// What the scan produced for one file.
enum Outcome {
    Reuse,
    /// Parsed from `offset`; `entries` are only the new tail.
    Resumed(u64, Vec<String>, Vec<CachedCall>),
    /// Parsed from 0; `entries` replace any cached ones.
    Full(u64, Vec<String>, Vec<CachedCall>),
}

pub fn load(dir: &Path, mp: &MultiProgress) -> Result<SourceOut> {
    let t0 = std::time::Instant::now();
    let scope = [dir];
    let mut cache: HashMap<PathBuf, Entry<()>> = filecache::load("claude-files", &scope);

    let files: Vec<(PathBuf, u64)> = walkdir::WalkDir::new(dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|x| x == "jsonl"))
        .filter_map(|e| e.metadata().ok().map(|m| (e.into_path(), m.len())))
        .collect();

    // Phase 1: classify every file (stat + probe only — no parsing).
    let planned: Vec<(PathBuf, u64, Plan<()>)> = files
        .par_iter()
        .map(|(path, len)| {
            (
                path.clone(),
                *len,
                filecache::plan(path, *len, cache.get(path)),
            )
        })
        .collect();
    let todo_bytes: u64 = planned
        .iter()
        .map(|(_, len, p)| filecache::plan_bytes(*len, p))
        .sum();

    // Phase 2: parse what needs parsing, with a progress bar when the work
    // is big enough to feel (cold scans read gigabytes).
    let pb = (todo_bytes >= filecache::BAR_MIN_BYTES).then(|| {
        let pb = mp.add(ProgressBar::new(todo_bytes));
        pb.set_style(
            ProgressStyle::with_template(
                "{spinner:.cyan} {msg} {wide_bar:.cyan/blue} {bytes}/{total_bytes}",
            )
            .expect("static template"),
        );
        pb.set_message("claude: parsing transcripts");
        pb
    });
    let results: Vec<(PathBuf, Option<Outcome>)> = planned
        .into_par_iter()
        .map(|(path, len, plan)| {
            let (oc, done) = match plan {
                Plan::Reuse => (Some(Outcome::Reuse), 0),
                Plan::Resume(offset, ()) => {
                    // seed from the cached dict so tail entries reuse
                    // existing string indices
                    let dict = cache.get(&path).map(|e| e.dict.clone()).unwrap_or_default();
                    (
                        parse_file(&path, offset, dict)
                            .ok()
                            .map(|(c, d, e)| Outcome::Resumed(c, d, e)),
                        len - offset,
                    )
                }
                Plan::Full => (
                    parse_file(&path, 0, Vec::new())
                        .ok()
                        .map(|(c, d, e)| Outcome::Full(c, d, e)),
                    len,
                ),
            };
            if let Some(pb) = &pb {
                pb.inc(done);
            }
            (path, oc)
        })
        .collect();
    if let Some(pb) = pb {
        pb.finish_and_clear();
    }

    let mut new_cache: HashMap<PathBuf, Entry<()>> = HashMap::with_capacity(results.len());
    let mut parsed_new = 0usize;
    for (path, oc) in results {
        let Some(oc) = oc else { continue }; // unreadable file: dropped
        let (offset, dict, mut tail, keep_old) = match oc {
            Outcome::Reuse => {
                if let Some(e) = cache.remove(&path) {
                    new_cache.insert(path, e);
                }
                continue;
            }
            Outcome::Resumed(o, d, t) => (o, d, t, true),
            Outcome::Full(o, d, t) => (o, d, t, false),
        };
        // a resumed parse starts from the file's cached dict so existing
        // indices stay valid; a full parse rebuilds it
        let mut entries = if keep_old {
            cache.remove(&path).map(|e| e.entries).unwrap_or_default()
        } else {
            cache.remove(&path);
            Vec::new()
        };
        entries.append(&mut tail);
        new_cache.insert(
            path.clone(),
            Entry {
                offset,
                mtime: filecache::mtime(&path),
                boundary: filecache::boundary(&path, offset),
                state: (),
                dict,
                entries,
            },
        );
        parsed_new += 1;
    }
    // rewrite the cache only when something changed — leftovers in `cache`
    // are files deleted since the last run
    if parsed_new > 0 || !cache.is_empty() {
        filecache::save("claude-files", &scope, &new_cache);
    }

    tracing::debug!(parsed_new, todo_bytes, elapsed = ?t0.elapsed(), "claude walk+parse done");

    // iterate in walk order — deterministic across runs
    let t1 = std::time::Instant::now();
    let mut seen = HashSet::new();
    let mut calls = Vec::new();
    let mut skipped_dupes = 0usize;
    let mut total_entries = 0usize;
    for (path, _) in &files {
        let Some(e) = new_cache.get(path) else {
            continue;
        };
        let dict: Vec<Arc<str>> = e.dict.iter().map(|s| s.as_str().into()).collect();
        for c in &e.entries {
            total_entries += 1;
            if seen.insert(c.key) {
                calls.push(c.to_call("claude", &dict));
            } else {
                skipped_dupes += 1;
            }
        }
    }
    tracing::debug!(
        files = files.len(),
        parsed_new,
        total_entries,
        calls = calls.len(),
        merge = ?t1.elapsed(),
        elapsed = ?t0.elapsed(),
        "claude source"
    );

    let mut note = format!(
        "claude: {} transcript files · {parsed_new} reparsed",
        files.len()
    );
    if skipped_dupes > 0 {
        note.push_str(&format!(" · {skipped_dupes} dupes skipped"));
    }
    Ok(SourceOut { calls, note })
}
